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
        let items = sessions::rail_menu_entries(favorite, topics_on)
            .into_iter()
            .map(|entry| {
                let kind = entry.kind;
                let pick = cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.sessions.rail_menu = None;
                    match kind {
                        sessions::MenuKind::Pin => this.toggle_favorite(id, cx),
                        sessions::MenuKind::Rename => this.start_rename(id, cx),
                        sessions::MenuKind::EnableTopics => this.enable_topics(id, cx),
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
        let mut editors = vec![self.editor.clone()];
        // Every leaf's composer, read through its handle (#2805).
        for pane in self.pane_states.values() {
            editors.push(pane.read(cx).editor.clone());
        }
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
                (self.turn.busy && self.transcript.session == Some(id))
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
