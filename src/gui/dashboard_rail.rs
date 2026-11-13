//! The rail: its menus, the activity maps behind its rows and the refresh
//! that keeps them current. Opening and closing it lives here too, because
//! the width it takes is the same state the rows read.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    pub(crate) fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        let open = !self.rail_open;
        self.set_rail_open(open, cx);
    }

    pub(crate) fn set_rail_open(&mut self, open: bool, cx: &mut Context<Self>) {
        set_rail_open_state(
            open,
            &mut self.rail_open,
            &mut self.rail_focused,
            &mut self.sessions,
        );
        // Closing bumps the deep-search generation so a query still in flight
        // bows out instead of repopulating a rail that is no longer shown
        // (#2720).
        if !open {
            self.rail_find_gen = self.rail_find_gen.wrapping_add(1);
        }
        // Both directions move the keyboard: opening hands it to the list,
        // closing gives it back to the input (#2665).
        self.refocus_screen();
        cx.notify();
    }

    /// The context-menu popover for one rail row (or topic chip), built only
    /// when that id's menu is open. Shared by full rows and chips so a chip's
    /// right-click offers the same pin / rename / split / delete picks.
    ///
    /// The card opens at the press that asked for it (#2690). Rows and chips
    /// both record their press point, so the card lands under the pointer
    /// rather than beside the rail: a chip sits mid-row, and the row's right
    /// edge is not where the click happened.
    pub(crate) fn rail_menu_popover_for(
        &self,
        id: Uuid,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.sessions.rail_menu != Some(id) {
            return None;
        }
        let favorite = self.sessions.is_favorite(id);
        let topics_on = self.sessions.is_topic_enabled(id);
        let is_topic = self.sessions.parents.contains_key(&id);
        let items = sessions::rail_menu_entries(favorite, topics_on, is_topic)
            .into_iter()
            .map(|entry| {
                let kind = entry.kind;
                let pick = cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.sessions.rail_menu = None;
                    match kind {
                        sessions::MenuKind::Pin => this.toggle_favorite(id, cx),
                        sessions::MenuKind::Rename => this.start_rename(id, cx),
                        // Assign project... is emitted only for parent sessions.
                        sessions::MenuKind::AssignProject => {
                            this.open_session_project_assignment(id, cx)
                        }
                        sessions::MenuKind::EnableTopics => this.enable_topics(id, cx),
                        // Turning topics off folds the family into this row,
                        // so the menu only arms the dialog (#2863).
                        sessions::MenuKind::DisableTopics => this.request_disable_topics(id, cx),
                        sessions::MenuKind::NewTopic => {
                            // The prompt hangs the topic under the pane
                            // holding the caret, so the row opens there first
                            // and the prompt only follows a pick that landed
                            // (#2397, #2819).
                            if this.open_picked_in_focused_pane(id, true, cx) {
                                this.start_topic_draft(cx);
                            }
                        }
                        sessions::MenuKind::GroupTopics => this.start_group_topics(id, cx),
                        sessions::MenuKind::SplitSide => this.split_row(id, Layout::Side, cx),
                        sessions::MenuKind::SplitStack => this.split_row(id, Layout::Stack, cx),
                        sessions::MenuKind::Delete => this.request_delete(id, cx),
                    }
                });
                sessions::menu_item(entry.label).on_click(pick)
            })
            .collect::<Vec<_>>();
        // The press point is the anchor: the card opens under the pointer
        // that asked for it, whichever row or chip that was (#2690).
        let at = self
            .rail_menu_at
            .unwrap_or(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        Some(sessions::menu_popover("rail-context-menu", items, at))
    }

    /// Drop every composer's context menu: a press outside the card is the
    /// press that closes it (#2749). Every field is checked, because a split
    /// pane has its own and only one of them opened the card.
    pub(crate) fn close_composer_menus(&mut self, cx: &mut Context<Self>) {
        let mut editors = vec![self.composer.editor.clone()];
        // Every leaf's composer, read through its handle (#2805).
        editors.extend(self.pane_editors(cx));
        for editor in editors {
            editor.update(cx, |editor, cx| editor.close_context_menu(cx));
        }
    }

    /// The rail's badges for every row it is about to draw (#2636).
    ///
    /// Resolved in one pass before the rows are built: the plan index needs
    /// `&mut self` and the row iterator holds `&self`, so the two cannot
    /// interleave. Evidence is cached by mtime, so this costs one `stat` per
    /// session that has a plan file and nothing for the rest.
    pub(crate) fn rail_activity_map(&mut self, cx: &App) -> HashMap<Uuid, rail_activity::RowState> {
        let rows: Vec<Uuid> = self.sessions.ordered().map(|s| s.id).collect();
        // The sweep throttle reads a moving clock; the animations read the
        // window's fixed origin. Two different questions, two clocks.
        let now = std::time::Instant::now();
        // One bridge lookup for the whole pass, not one per row: a detached
        // command outlives its turn, so it is the only trace that session still
        // has work in flight (#762).
        let manager = agent_bridge::bridge().and_then(|b| b.service.background_manager());
        let mut map = HashMap::with_capacity(rows.len());
        for id in rows {
            let live = self.rail.is_live(
                id,
                // The open session's turn, plus any turn parked under its own
                // session: a switched-away turn keeps running, so its rail row
                // must keep its spinner instead of claiming idle work (#2616).
                // Split leaves keep their own flag. A parked card that has
                // ended is NOT live (#2811): the entry outlives the turn so the
                // stop survives the switch, and reading presence as work would
                // spin a row for a turn nobody is running.
                (self.turn.busy && self.chat_session == Some(id))
                    || self.stashed.get(&id).is_some_and(|parked| parked.busy),
                self.pane_states
                    .get(&id)
                    .is_some_and(|pane| pane.read(cx).busy),
                manager.as_ref().is_some_and(|mgr| mgr.running_for(id) > 0),
            );
            let plan = self.rail_index.evidence(id, live, now);
            let seen = plan.is_some_and(|evidence| self.rail_seen.is_seen(id, evidence.stamp));
            map.insert(id, rail_activity::row_state(live, plan, seen));
        }
        map
    }

    /// The rail card's plan progress for every row it is about to draw:
    /// `(done, total)` for an approved plan with a checklist, nothing else.
    ///
    /// A separate pass from the badges because the idle repaint check only
    /// asks about animation, and this question is static state. The index
    /// already cached the paths and documents for the badge pass, so each row
    /// here costs one `stat` at most and a parse only when the file moved.
    /// Snapshot every session's plan progress before the rail filters (#2719).
    /// The plan chip in `visible()` reads this map, so it must hold a row's
    /// counts even when that row is about to be hidden: filling it from the
    /// filtered `ordered()` would chase its own tail. The index resolves on
    /// demand, so iterating all items warms it for the badge pass that follows.
    pub(crate) fn snapshot_rail_plan_progress(&mut self) {
        let now = std::time::Instant::now();
        let ids: Vec<Uuid> = self.sessions.items.iter().map(|s| s.id).collect();
        self.sessions.plan_progress.clear();
        for id in ids {
            if let Some(progress) = self.rail_index.progress(id, now) {
                self.sessions.plan_progress.insert(id, progress);
            }
        }
    }

    /// The plan bar's counts for every rail row, taken from the snapshot the
    /// frame already built.
    ///
    /// Resolving them a second time walked every session again, one `stat` and
    /// doc lookup each, for a map the frame already had (#2805). The snapshot
    /// is keyed on all sessions, so it covers every row `ordered()` can yield.
    pub(crate) fn rail_plan_progress_map(&self) -> HashMap<Uuid, (usize, usize)> {
        self.sessions.plan_progress.clone()
    }

    /// The detached commands every rail row is running, formatted once per
    /// frame: `⏳ cargo test 32s` per session, `None` when nothing runs.
    ///
    /// The rail card answers "is anything still going in that conversation"
    /// without opening it, which is the one fact a streaming spinner cannot
    /// carry: a detached command takes the turn idle, so the row would
    /// otherwise read as finished while a build grinds on (#762).
    ///
    /// Shaped by [`opencrabs::tui::render::format_background_tasks`], the same
    /// formatter the composer strip and the terminal's footer use, so one
    /// running task cannot read three different ways. One manager lookup for
    /// the whole pass, never one per row.
    pub(crate) fn rail_running_map(&self) -> HashMap<Uuid, SharedString> {
        let Some(manager) = agent_bridge::bridge().and_then(|b| b.service.background_manager())
        else {
            return HashMap::new();
        };
        let mut map = HashMap::new();
        for id in self.sessions.ordered().map(|s| s.id) {
            let tasks: Vec<(String, u64)> = manager
                .running_tasks(id)
                .into_iter()
                .map(|t| (t.label, t.started.elapsed().as_secs()))
                .collect();
            if let Some(line) = opencrabs::tui::render::format_background_tasks(
                &tasks,
                opencrabs::tui::render::LABEL_CHARS,
            ) {
                map.insert(id, SharedString::from(format!("\u{23f3} {line}")));
            }
        }
        map
    }

    /// Refetch the rail from the ledger. The pinned set rides along: pins come
    /// from a sidecar the TUI also writes, so a list refresh that skipped it
    /// would draw a row in the wrong slot until the next pin click.
    pub(crate) fn refresh_rail(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let result = sessions::load_sessions(db_path.clone()).await;
            // One batched lookup per refresh pass, never per row per frame
            // (#2490): the topic store outranks the stored session name and a
            // rename has to reach the rail without a restart.
            let titles = match &result {
                Ok(items) => sessions::load_telegram_topic_titles(db_path, items).await,
                Err(_) => Ok(HashMap::new()),
            };
            this.update(cx, |this, cx| {
                match titles {
                    Ok(pairs) => this.sessions.topic_titles = pairs,
                    Err(e) => logging::error(&format!("topic titles refresh: {e}")),
                }
                match result {
                    Ok(items) => {
                        this.sessions.items = items;
                        this.sessions.error = None;
                        // A refetch can drop the row the cursor is on; a
                        // cursor pointing at a ghost would arm nothing.
                        if !this.sessions.cursor_visible() {
                            this.sessions.cursor = None;
                        }
                    }
                    Err(e) => this.sessions.error = Some(e.into()),
                }
                this.refresh_favorites(cx);
                // Previews follow the rows, so a new or renamed session gets
                // its line without a second trigger.
                this.refresh_previews(cx);
                // The refetched rows carry the names a pane's topic strip
                // draws, so the panes are told alongside the rail (#2805).
                this.notify_panes(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One batched preview read for the rows the rail already holds.
    ///
    /// Never called from a paint: the row loop reads `sessions.previews`. The
    /// rail repaints on a shared clock, so asking per row there would be a
    /// query per card per frame.
    pub(crate) fn refresh_previews(&mut self, cx: &mut Context<Self>) {
        // One read at a time (#2817). A rail refetch and a streaming tick can
        // land in the same frame, and each one spawns a thread with its own
        // runtime and its own connection; on a busy ledger both waited long
        // enough to overlap. A request that arrives mid-read is remembered and
        // re-run when the read lands, so a new session still gets its line.
        if self.previews_in_flight.get() > 0 {
            self.previews_dirty.set(true);
            return;
        }
        let ids: Vec<uuid::Uuid> = self.sessions.items.iter().map(|s| s.id).collect();
        if ids.is_empty() {
            self.sessions.previews.clear();
            return;
        }
        self.previews_in_flight
            .set(self.previews_in_flight.get().saturating_add(1));
        let in_flight = self.previews_in_flight.clone();
        let dirty = self.previews_dirty.clone();
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // Released on drop, so a task whose view went away still frees the
            // slot instead of wedging previews for the rest of the run.
            let _slot = PreviewSlot(in_flight.clone());
            match sessions::load_previews(db_path, ids).await {
                Ok(pairs) => {
                    this.update(cx, |this, cx| {
                        this.sessions.previews = pairs;
                        // A request that arrived while this one ran is served
                        // now, from the same completion, never from a timer.
                        if dirty.replace(false) {
                            this.refresh_previews(cx);
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    dirty.set(false);
                    logging::error(&format!("rail previews: {e}"));
                }
            }
        })
        .detach();
    }

    /// Keep a running conversation's preview warm without hammering the DB:
    /// at most one read a second, and only while a turn can still move the
    /// line. The chunk and tool events are the triggers, so no timer is added
    /// for this.
    pub(crate) fn refresh_previews_if_live(&mut self, cx: &mut Context<Self>) {
        // A parked turn is still working even though its progress is not
        // reaching the rail set.
        let stashed_live = self
            .stashed
            .values()
            .any(|t| t.live.user.is_some() || !t.live.tools.is_empty());
        if !previews_can_move(self.turn.busy, stashed_live, self.rail.streaming_count()) {
            return;
        }
        if self
            .preview_refreshed_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.preview_refreshed_at = Some(std::time::Instant::now());
        self.refresh_previews(cx);
    }

    /// ctrl+f's deep search (#2720): let the typing settle, then read which
    /// sessions mention the query in their messages, off the paint path. Each
    /// call bumps the generation and the spawned query carries it; a result
    /// only lands when the generation still matches, so the newest text wins
    /// even when an older query finishes last (the notice timer's trick,
    /// editor.rs). An empty field clears the hits at once and spawns nothing.
    pub(crate) fn schedule_rail_find(&mut self, cx: &mut Context<Self>) {
        let query = self.sessions.filter.trim().to_string();
        self.rail_find_gen = self.rail_find_gen.wrapping_add(1);
        let generation = self.rail_find_gen;
        if query.is_empty() {
            self.sessions.content_matches.clear();
            cx.notify();
            return;
        }
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // A burst of keystrokes costs one query, not one per character.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            // A newer keystroke or a closed rail already moved the generation
            // on: this query is stale, so it never touches the database.
            let stale = this
                .update(cx, |this, _| this.rail_find_gen != generation)
                .unwrap_or(true);
            if stale {
                return;
            }
            match sessions::load_content_matches(db_path, query).await {
                Ok(rows) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.rail_find_gen == generation {
                            this.apply_rail_find(rows);
                            cx.notify();
                        }
                    });
                }
                Err(e) => logging::error(&format!("rail find: {e}")),
            }
        })
        .detach();
    }

    /// Fold the content rows into the rail's deep-hit map (#2720): title
    /// matches lead in ledger order, content-only sessions follow with their
    /// snippet, and a session found both ways appears once. Only the
    /// snippet-bearing rows are kept here; `visible()` already holds the title
    /// matches on screen through `matches`, so this map is purely the deep
    /// hits and the snippets that stand in for their previews.
    pub(crate) fn apply_rail_find(
        &mut self,
        content: Vec<opencrabs::db::repository::ContentSearchRow>,
    ) {
        let title_ids: Vec<Uuid> = self
            .sessions
            .items
            .iter()
            .filter(|s| sessions::matches(s, &self.sessions.filter))
            .map(|s| s.id)
            .collect();
        let merged = sessions::merge_find_results(title_ids, &content);
        self.sessions.content_matches = merged
            .into_iter()
            .filter_map(|m| m.snippet.map(|snippet| (m.session_id, snippet)))
            .collect();
    }

    /// Schedule a deep read only when the trimmed query actually changed
    /// (#2721). Both typing (finder_key) and the editing chords
    /// (apply_finder_edit) funnel here, while caret moves and arrows leave the
    /// query untouched and so never rebuild the catalog.
    pub(crate) fn seeker_find_if_changed(&mut self, cx: &mut Context<Self>) {
        let query = self
            .finder
            .as_ref()
            .map(|state| state.query().trim().to_string())
            .unwrap_or_default();
        if query == self.seeker_scheduled_query {
            return;
        }
        self.seeker_scheduled_query = query;
        self.schedule_seeker_find(cx);
    }

    /// Seeker deep search (#2721): let the typing settle, then read which
    /// sessions mention the query in their messages and rebuild the card's
    /// rows with the snippets. Same generation guard as the rail's ctrl+f, so
    /// the newest query wins even when an older read finishes last. A cached
    /// query rebuilds at once; an empty query needs no read.
    pub(crate) fn schedule_seeker_find(&mut self, cx: &mut Context<Self>) {
        let query = self
            .finder
            .as_ref()
            .map(|state| state.query().trim().to_string())
            .unwrap_or_default();
        if query.is_empty() {
            return;
        }
        if self.seeker_find.contains_key(&query) {
            self.refresh_seeker_items(cx);
            return;
        }
        self.seeker_find_gen = self.seeker_find_gen.wrapping_add(1);
        let generation = self.seeker_find_gen;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            // A burst of keystrokes costs one query, not one per character.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let stale = this
                .update(cx, |this, _| this.seeker_find_gen != generation)
                .unwrap_or(true);
            if stale {
                return;
            }
            match sessions::load_content_matches(db_path, query.clone()).await {
                Ok(rows) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.seeker_find_gen == generation {
                            this.seeker_find.insert(query, rows);
                            this.refresh_seeker_items(cx);
                        }
                    });
                }
                Err(e) => logging::error(&format!("seeker find: {e}")),
            }
        })
        .detach();
    }

    /// Rebuild the open Seeker's rows from the current catalog (which now
    /// folds in any cached content hits) without touching the query or caret.
    pub(crate) fn refresh_seeker_items(&mut self, cx: &mut Context<Self>) {
        let items = self.finder_items();
        if let Some(state) = self.finder.as_mut() {
            state.refresh_items(items);
        }
        cx.notify();
    }
}

impl Dashboard {
    /// The rail column, drawn from the frame this window resolved for it
    /// (#2933).
    ///
    /// The window used to build the whole column inline in `render`: every row,
    /// every topic chip, every badge and the filter menu, each row minting its
    /// own listeners, and then it threw all of it away at the layout pass. A
    /// keystroke re-renders the window, so that work was paid per character
    /// typed. The column is a cached view now: the window hands it one frame of
    /// plain data and a frame that compares equal replays the last prepaint.
    pub(crate) fn rail_element(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let frame = self.rail_frame(cx);
        let view = self.rail_view.clone();
        view.update(cx, |rail, cx| rail.set_frame(frame, cx));
        // The same box the view draws with, handed to the cached path: the
        // reserved box and the drawn one cannot drift (#2805).
        AnyView::from(view)
            .cached(rail_view::rail_root_style(self.rail_width))
            .into_any_element()
    }

    /// Advance the rail's clock from the window's poll (#2933).
    ///
    /// The rail's spinner and a waiting plan's ring move on their own, so they
    /// ask for a repaint on their own. The window is not notified: the
    /// transcripts beside the rail replay their last prepaint instead of being
    /// rebuilt for a glyph.
    pub(crate) fn rail_tick(&mut self, cx: &mut Context<Self>) {
        let view = self.rail_view.clone();
        view.update(cx, |rail, cx| rail.tick(cx));
    }

    /// Resolve everything the rail paints, as plain comparable data (#2933).
    ///
    /// Every map below is resolved here, once, exactly as the inline render
    /// resolved it. Nothing in this frame is an element, a `Session` or a clock
    /// phase: the frame decides whether the rail is notified, so two frames
    /// that would paint the same pixels have to compare equal.
    pub(crate) fn rail_frame(&mut self, cx: &mut Context<Self>) -> rail_view::RailFrame {
        // With the rail closed the frame still resolves every map under it and
        // then paints nothing, so the work is skipped rather than repeated.
        let rail_drawn = self.rail_open || self.screen == Screen::Onboard;
        // The plan chip filters rows by progress, so resolve every session's
        // counts before any pass calls `visible()` (#2719). This also warms the
        // index the badge and bar passes read below.
        if rail_drawn {
            self.snapshot_rail_plan_progress();
        }
        // What every row says about work in flight (#2636), resolved once
        // before the loop: the index needs `&mut self` and the row iterator
        // holds `&self`.
        let activity = if rail_drawn {
            self.rail_activity_map(cx)
        } else {
            HashMap::new()
        };
        // The plan bar reads the same cached index the badges just warmed: one
        // stat per row with a plan file, no second loader.
        let plan_progress = if rail_drawn {
            self.rail_plan_progress_map()
        } else {
            HashMap::new()
        };
        // Detached commands say "work is still running here" even when the turn
        // is idle, so every card answers it without being opened.
        let running_tasks = if rail_drawn {
            self.rail_running_map()
        } else {
            HashMap::new()
        };
        // One clock for the whole pass, so every flashing card blinks on the
        // same frame instead of one behind its neighbour.
        let flash_now = std::time::Instant::now();
        // Telegram-style topics: a parent's children paint as compact chips on
        // its card, not as their own rows. Resolve the chip set once before the
        // loop so the paint can skip those rows and hang the strip on the
        // parent. A live search falls back to flat rows so a match is never
        // hidden inside a chip; a favorite stays a chip on its parent, never a
        // row. Chips always paint: a terminal-written `topics_collapsed` flag
        // never blanks the strip, the GUI has no row-era fold (#2701).
        let searching = !self.sessions.filter.trim().is_empty();
        let visible_ids: std::collections::HashSet<Uuid> = if rail_drawn {
            self.sessions.visible().map(|s| s.id).collect()
        } else {
            std::collections::HashSet::new()
        };
        // The grouped stamp, spend and indent of every card, one pass each. The
        // row loop asked per row and every ask scanned the whole rail, so a
        // 200-row rail paid ~80k item visits a frame for numbers that only
        // change when a message lands (#2817).
        let (grouped_stamps, grouped_costs, indented_ids) = if rail_drawn {
            (
                self.sessions.grouped_activities(),
                self.sessions.grouped_costs(),
                self.sessions.indented_ids(),
            )
        } else {
            (
                HashMap::new(),
                HashMap::new(),
                std::collections::HashSet::new(),
            )
        };
        let chip_children: std::collections::HashSet<Uuid> = if searching || !rail_drawn {
            std::collections::HashSet::new()
        } else {
            self.sessions
                .visible()
                .filter_map(|s| self.sessions.parents.get(&s.id).map(|p| (s.id, *p)))
                .filter(|(_, p)| visible_ids.contains(p))
                .map(|(cid, _)| cid)
                .collect()
        };
        let mut children_by_parent: HashMap<Uuid, Vec<sessions::RailChip>> = HashMap::new();
        // One clock for the chip order (#2853): a topic with work in flight
        // sorts as if it just spoke, so the chips the reader can see working
        // lead the strip instead of holding the slot the last ledger load gave
        // them. Every busy chip ties on this one instant, which keeps their
        // existing order stable rather than reshuffling each frame.
        let chips_now = chrono::Utc::now();
        let chip_rows: Vec<&opencrabs::db::models::Session> = if rail_drawn {
            self.sessions.visible().collect()
        } else {
            Vec::new()
        };
        for s in chip_rows {
            if chip_children.contains(&s.id)
                && let Some(p) = self.sessions.parents.get(&s.id)
            {
                children_by_parent
                    .entry(*p)
                    .or_default()
                    .push(sessions::RailChip {
                        id: s.id,
                        // A chip names its own topic, never the chat plus the
                        // topic plus the channel key (#2766): the card above it
                        // already says the chat.
                        title: rail::chip_name(
                            s.title.as_deref(),
                            self.sessions.topic_titles.get(&s.id).map(String::as_str),
                        ),
                        // The clock the card's own stamp folds over (#2815), so
                        // the strip and the title can never disagree.
                        at: s.updated_at,
                        // The marker the chip paints: a live turn or a running
                        // plan task (rail_activity::row_state).
                        busy: activity.get(&s.id).is_some_and(|st| st.badge.is_some()),
                    });
            }
        }
        for kids in children_by_parent.values_mut() {
            // The strip's order (#2853): the topics with work in flight lead,
            // then the rest by the stamp the card prints. Without this the chips
            // rode whatever order the last ledger load returned, and that load
            // only runs at turn boundaries, so a topic that had just started
            // working kept the slot it held minutes earlier while its chip
            // already showed the spinner.
            kids.sort_by_key(|chip| sessions::chip_order_key(chip, chips_now));
        }
        // Pinned rows first, the rest untouched: the TUI's own order.
        let rail_order: Vec<&opencrabs::db::models::Session> = if rail_drawn {
            self.sessions.ordered().collect()
        } else {
            Vec::new()
        };
        let mut rows: Vec<rail_view::RailRow> = Vec::with_capacity(rail_order.len());
        for s in rail_order {
            let id = s.id;
            // This child paints as a chip on its parent's card, not a row.
            if chip_children.contains(&id) {
                continue;
            }
            let selected = self.sessions.selected == Some(id);
            let cursored = self.sessions.cursor == Some(id);
            let favorite = self.sessions.is_favorite(id);
            let indented = indented_ids.contains(&id);
            let closed = self.sessions.is_closed(id);
            // One glyph cell, from the window's shared clock, so a list of busy
            // rows blinks as one spinner instead of ten (#2636).
            let mark = activity.get(&id).copied().unwrap_or_default();
            // A selected topic owns its parent's existing activity/preview slot
            // (#2804). Cursor, plan progress and running tasks stay with the
            // parent.
            let progress_id = children_by_parent.get(&id).map_or(id, |kids| {
                self.sessions.progress_id(id, kids.iter().map(|kid| kid.id))
            });
            // The stored icon leads the row (#2790), resolved exactly like the
            // topic tab resolves it: an embedded mark, a custom image from the
            // view's cache, or a legacy glyph. The row adds the channel mark
            // beside it (#2800). A pick in the rename dialog reaches the card
            // without a restart, and the paint never reads a file.
            let icon = self
                .sessions
                .topic_icons
                .get(&id)
                .and_then(|stored| self.rail_icon(stored));
            let chips = children_by_parent.get(&id).map(|kids| rail_view::RailChips {
                expanded: self.rail_chips_expanded.contains(&id),
                kids: kids
                    .iter()
                    .map(|kid| {
                        let mark = activity.get(&kid.id).copied().unwrap_or_default();
                        rail_view::RailTopic {
                            id: kid.id,
                            title: SharedString::from(kid.title.clone()),
                            mark,
                            selected: self.sessions.selected == Some(kid.id)
                                || self.sessions.cursor == Some(kid.id),
                            plan: plan_progress.get(&kid.id).copied(),
                        }
                    })
                    .collect(),
            });
            rows.push(rail_view::RailRow {
                card: sessions::RailCard::of(s),
                favorite,
                flags: rail_view::RailRowFlags {
                    selected,
                    cursored,
                    indented,
                    closed,
                    mark,
                    // Already formatted in the loader: the paint only clones a
                    // shared string, so the shared clock costs no allocation.
                    // While ctrl+f is open a deep hit's snippet stands in for
                    // the preview so the reader sees why the row matched
                    // (#2720). Idle rows read the DB preview.
                    preview: self
                        .sessions
                        .content_matches
                        .get(&progress_id)
                        .filter(|_| self.sessions.filter_active)
                        .map(|snippet| SharedString::from(snippet.clone()))
                        .or_else(|| self.sessions.previews.get(&progress_id).cloned()),
                    // A turn nobody has a pane for still gets a line on its card
                    // (#2718). The wording is the pane's own (#2326), so a split
                    // leaf and its rail row never describe the same turn two
                    // different ways.
                    activity: self.rail_activity_for(progress_id, cx),
                    plan: plan_progress.get(&id).copied(),
                    // The bar blinks orange for a few seconds after a task in
                    // this session's plan settles, on the same event that rings
                    // the chime, then falls back to its resting teal.
                    flash: opencrabs::utils::completion_sound::flash_blink_on(
                        self.plan_flash_until.get(&id).copied(),
                        flash_now,
                    ),
                    // The card's last line: whatever detached command is still
                    // running here, formatted exactly as the composer strip
                    // does.
                    running: running_tasks.get(&id).cloned(),
                    // A card stands for its topics, so its stamp adds their
                    // spend (#2807). A topic keeps its own ledger row and paints
                    // no stamp of its own, which makes the card the only place
                    // the money under it can be summed.
                    cost: grouped_costs.get(&id).copied().unwrap_or(s.total_cost),
                    // A card stands for its topics here too (#2815): a message
                    // written to a topic touches only that topic's ledger row,
                    // so the card's own stamp would freeze at the last message
                    // addressed to the card itself while its chips ran minutes
                    // ago. Folded over the same children the cost is.
                    stamp_at: grouped_stamps.get(&id).copied().unwrap_or(s.updated_at),
                    icon,
                },
                chips,
            });
        }
        // The channel chip (#2452): quiet notice plus the one action, drawn at
        // the end of the rail only while a configured channel has no live owner.
        // `Start daemon` runs the onboarding installer once; the probe loop
        // flips the chip off when the daemon is live.
        let footer = self.channel_chip.clone().map(|line| rail_view::RailFooter {
            line,
            error: self.daemon_error.clone(),
            action: SharedString::from(if self.daemon_starting {
                "starting..."
            } else {
                "Start daemon"
            }),
        });
        rail_view::RailFrame {
            width_rem: self.rail_width,
            rem: self.rem,
            head: sessions::RailHead {
                filter: SharedString::from(self.sessions.filter.clone()),
                filter_active: self.sessions.filter_active,
                error: self.sessions.error.clone(),
                matched: self.sessions.matched(),
                items: self.sessions.items.len(),
            },
            rows,
            footer,
            // The filter menu's own state (#2719, #2787), which lives on
            // `sessions` so it survives the repaint and resets when the rail
            // closes.
            filter: self.sessions.rail_filter,
            filter_open: self.sessions.rail_filter_open,
        }
    }
}

/// What a press or a click in the rail asks of the window (#2933).
///
/// These lived as closures inside the window's render, which is why the column
/// could not be cached: a closure can only be minted where the state it writes
/// is in scope. The rail now asks the window by name, so the row it draws can be
/// built once and replayed until the frame it paints actually changes.
impl Dashboard {
    /// Right-click on a card: open that row's menu (#2354).
    pub(crate) fn rail_row_menu(&mut self, id: Uuid, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.sessions.rail_menu = Some(id);
        // Where the card opens from: the row's own height, so it lands beside
        // the row instead of over it (#2742).
        self.rail_menu_at = Some(at);
        // The card takes the pointer, not the keyboard: the composer keeps its
        // caret while the menu is open (#2798).
        self.rail_hands_back();
        cx.notify();
    }

    /// Right-click on a topic chip: the same menu, anchored at the chip.
    ///
    /// A chip sits inside its parent's row, so the press gives the card its
    /// height the same way a row's does (#2742), and the caret comes back from
    /// here rather than from the window root (#2798).
    pub(crate) fn rail_chip_menu(&mut self, id: Uuid, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.sessions.rail_menu = Some(id);
        self.rail_menu_at = Some(at);
        self.rail_hands_back();
        cx.notify();
    }

    /// The visible pin, which shares one favorite writer with the menu (#2803).
    pub(crate) fn rail_toggle_pin(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.rail_hands_back();
        self.toggle_favorite(id, cx);
    }

    /// A topic chip opens its child.
    ///
    /// A chip is a pick like any row: while a split waits, it fills the pane
    /// instead of opening the topic in the chat leaf (#2760).
    pub(crate) fn rail_chip_pick(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.close_rail_filter(cx);
        self.pick_or_open(id, cx);
    }

    /// A left press on a card: the start of the slide that opens its menu.
    pub(crate) fn rail_row_press(&mut self, id: Uuid, from_x: f32, cx: &mut Context<Self>) {
        self.rail_slide_opened = None;
        self.rail_swipe = Some((id, from_x));
        // A left press on the rail dismisses a context menu: the press is
        // elsewhere, so the menu goes with it (#2354).
        self.sessions.close_rail_menu();
        // The filter panel folds on a row press too (#2787): the reader picked a
        // chat, the chips have done their job.
        self.sessions.rail_filter_open = false;
        // Same rule for a transcript row's menu (#2613): the rail is outside the
        // transcript, so the press closes it too.
        self.msg_menu = None;
        self.msg_menu_row = None;
        self.msg_menu_leaf = None;
        // A press is not a pick yet, and the caret does not wait for one:
        // whatever the release does, the keys are the composer's from the press
        // on (#2798).
        self.rail_hands_back();
        cx.notify();
    }

    /// A click on a card: pick it.
    pub(crate) fn rail_row_pick(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // The release that ended a slide is the tail of that gesture, not a
        // pick: it leaves the strip it just opened alone.
        if self.rail_slide_opened.take() == Some(id) {
            cx.notify();
            return;
        }
        // A split waiting for its second session takes this row (#2438).
        if self.split_pick.is_some() {
            self.choose_split(id, cx);
            return;
        }
        // The find field is a jump aid: picking a row is its exit, and the
        // keyboard goes back to the conversation.
        self.close_rail_filter(cx);
        // The pane holding the caret takes the pick (#2819).
        self.open_picked_in_focused_pane(id, true, cx);
    }

    /// The "+N" pill: the rest of a card's topics paint as chips.
    pub(crate) fn rail_expand_chips(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.rail_chips_expanded.insert(id);
        cx.notify();
    }

    /// The collapse pill: back to three chips.
    pub(crate) fn rail_collapse_chips(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.rail_chips_expanded.remove(&id);
        cx.notify();
    }

    /// The channel chip's one action (#2452): run the installer once.
    pub(crate) fn rail_start_daemon(&mut self, cx: &mut Context<Self>) {
        self.start_daemon(cx);
    }

    /// The funnel: open or close the rail filter menu (#2787).
    pub(crate) fn rail_toggle_filter_menu(&mut self, cx: &mut Context<Self>) {
        self.sessions.rail_filter_open = !self.sessions.rail_filter_open;
        cx.notify();
    }

    /// One plan-state chip (#2719).
    pub(crate) fn rail_set_plan_filter(
        &mut self,
        plan: sessions::PlanFilter,
        cx: &mut Context<Self>,
    ) {
        self.sessions.rail_filter.plan = plan;
        cx.notify();
    }

    /// The cron/RSI toggle beside the plan chips.
    pub(crate) fn rail_toggle_automation(&mut self, cx: &mut Context<Self>) {
        self.sessions.rail_filter.show_automation = !self.sessions.rail_filter.show_automation;
        cx.notify();
    }
}
