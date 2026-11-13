//! Topics: the strip above a pane, drafts, group topics, the state a row
//! reads and the icon image it wears.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// The chat's menu (#2480): what can be done to the conversation it
    /// belongs to. It opens from a right press on the conversation or on a
    /// topic tab; the header button that used to open it went with the title
    /// row (#2596).
    ///
    /// The card floats over the chat as a layer. The old menu was a column of
    /// rows laid *inside* the topic strip, so opening it grew the strip and
    /// pushed the conversation down; the owner read that as a bare element
    /// appearing, not a menu (#2480).
    ///
    /// A right press on a topic tab opens this same card for that tab's
    /// session, so the actions follow the row that asked for them.
    pub(crate) fn chat_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.topic_menu?;
        let is_topic = self.sessions.parents.contains_key(&target);
        let mut items: Vec<Stateful<Div>> = Vec::new();
        if self.topic_draft_parent().is_some() {
            let new_topic = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                this.start_topic_draft(cx);
            });
            items.push(sessions::menu_item("New topic").on_click(new_topic));
        }
        let (view_label, edit_label, delete_label) = if is_topic {
            ("View topic info", "Edit topic", "Delete topic")
        } else {
            ("View info", "Rename", "Delete conversation")
        };
        let view = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.topic_menu = None;
            this.open_topic_info(target, cx);
        });
        items.push(sessions::menu_item(view_label).on_click(view));
        let edit = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.topic_menu = None;
            this.start_rename(target, cx);
        });
        items.push(sessions::menu_item(edit_label).on_click(edit));
        if is_topic {
            let close_label = if self.sessions.is_closed(target) {
                "Reopen topic"
            } else {
                "Close topic"
            };
            let close = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.topic_menu = None;
                this.toggle_closed_topic(target, cx);
            });
            items.push(sessions::menu_item(close_label).on_click(close));
        }
        let delete = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.topic_menu = None;
            this.request_delete(target, cx);
        });
        items.push(sessions::menu_item(delete_label).on_click(delete));
        // Under the press that opened it (#2690), through the same popover the
        // rail uses. The card used to sit at the column's top corner, which is
        // where the header used to be: not where the user clicked, and in a
        // split not even the pane whose tab was pressed.
        let at = self
            .topic_menu_at
            .unwrap_or(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        Some(sessions::menu_popover("chat-menu", items, at))
    }

    /// One pane's topic strip (#2686, #2687), or none when that pane does not
    /// carry the family. A pane builds this in its own render through the
    /// window handle, and `panes_body` builds the chat leaf's through the same
    /// function, so both wear one strip.
    pub(crate) fn pane_topics(
        &mut self,
        session: Uuid,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let leaves = self
            .panes
            .as_ref()
            .map(|tree| tree.leaves())
            .unwrap_or_default();
        let pane_root = self
            .sessions
            .parents
            .get(&session)
            .copied()
            .unwrap_or(session);
        // In a split the strip belongs to the pane that carries the family,
        // not to every topic inside it: a wall of topic panes repeated the
        // same bar once per pane (#2761).
        let carries_topics = leaves.len() == 1 || pane_root == session;
        // The flag is not enough (#2863): a family whose topics were all
        // archived or folded still carried it, and the strip painted a bar
        // whose only chip was `All`.
        if !carries_topics
            || !self.sessions.is_topic_enabled(pane_root)
            || !self.family_has_topics(pane_root)
        {
            return None;
        }
        let pane_key = session.to_string();
        let tabs = self.topic_tabs(Some(session), cx);
        let toggle_edge = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            cx.stop_propagation();
            this.cycle_topic_edge(cx);
        });
        Some(topic_strip(&pane_key, topic_tabs::Edge::Top, toggle_edge, tabs).into_any_element())
    }

    /// The tab strip for the open chat's topic family (#2400, #2480): `All`
    /// first, then each of its topics in list order. The open one is marked,
    /// and a click opens that session.
    ///
    /// The conversation's own name is not a chip. It used to lead the strip
    /// (`vec![root]`), which put the session in the row of its own topics and
    /// made it read as a sibling. The name belongs to the card above, and
    /// `All` is the way back to it.
    ///
    /// `pane` names the leaf the strip is drawn in, `None` for the unsplit
    /// window (#2686). Every read and every click resolves against it, so a
    /// pane's strip shows that pane's family and a click moves that pane.
    pub(crate) fn topic_tabs(&self, pane: Option<Uuid>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(open) = pane.or(self.chat_session) else {
            return Vec::new();
        };
        let root = self.sessions.parents.get(&open).copied().unwrap_or(open);
        if !self.sessions.is_topic_enabled(root) {
            return Vec::new();
        }
        // The topic store outranks the stored session name (#2490): a rename
        // in Telegram reaches this label on the next refresh pass. With no live
        // name yet, the chip strips the row's platform prefix and channel key
        // instead of printing them (#2762).
        let name = |id: Uuid| {
            let stored = self
                .sessions
                .items
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.title.as_deref());
            rail::chip_name(
                stored,
                self.sessions.topic_titles.get(&id).map(String::as_str),
            )
        };
        let topics: Vec<Uuid> = self
            .sessions
            .items
            .iter()
            .filter(|s| self.sessions.parents.get(&s.id) == Some(&root))
            .map(|s| s.id)
            .collect();
        // A horizontal bar leads with the icon, a rail stacks it above the name
        // (#2480). Both edges hug their content, so one chip can never fill the
        // rail the way it did before.
        let shape = topic_bar::chip_shape(self.topic_edge.is_vertical());
        let icon_rems = topic_tabs::style(self.topic_edge.is_vertical()).topic_icon_rems;
        // A wide family folds its tail into one pill, and the open topic keeps
        // its chip either way (#2762).
        let chips: Vec<topic_bar::TopicChip> = topics
            .iter()
            .map(|id| topic_bar::TopicChip {
                id: *id,
                activity: self.topic_is_busy(*id, cx),
            })
            .collect();
        let plan = topic_bar::bar_plan(root, open, &chips, topic_bar::BAR_TOPICS_SHOWN);
        let mut cells: Vec<AnyElement> = Vec::with_capacity(plan.entries.len() + 1);
        for entry in plan.entries {
            let id = entry.session(root);
            let current = entry.is_open(root, open);
            let click = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                match pane {
                    Some(leaf) => this.open_topic_in_pane(leaf, id, cx),
                    None => this.open_session(id, cx),
                }
            });
            // A right press on a tab opens the same menu the `...` draws,
            // for the session that tab stands for (#2480). Before this it
            // opened nothing at all, and the only way to edit a topic was
            // to open it first and hunt the menu.
            let open_menu = cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.topic_menu = Some(id);
                // The press point, so the card opens under the click rather
                // than at the column's top corner (#2690).
                this.topic_menu_at = Some(ev.position);
                cx.notify();
            });
            // A busy topic is marked with a dot, the same signal the rail uses
            // for work in flight.
            let activity = chips.iter().any(|chip| chip.id == id && chip.activity);
            // `All` has the same icon slot and cell geometry as a topic.
            let (label, picture) = match entry {
                topic_bar::BarEntry::All => (
                    "All".to_string(),
                    topic_icons::tab_icon(topic_icons::store_name("general").as_str(), icon_rems),
                ),
                topic_bar::BarEntry::Topic(id) => {
                    // The icon sits before the name and is not a word, so the
                    // two-word shortening (#2477) is applied to the name alone.
                    // A stored picture is drawn; a stored emoji stays text (#2478).
                    // A custom image resolves from the view's cache before the
                    // text fallback, so an img: reference never prints as a
                    // label (#2790).
                    let stored = self.sessions.topic_icons.get(&id).cloned();
                    let picture = stored.as_deref().and_then(|stored| {
                        topic_icons::tab_icon(stored, icon_rems)
                            .or_else(|| self.custom_icon_picture(stored, icon_rems))
                    });
                    let glyph = match (&picture, stored.as_deref()) {
                        (None, Some(icon)) if !icon.starts_with(topic_icons::IMG_PREFIX) => {
                            Some(topic_icons::tab_glyph(icon, icon_rems))
                        }
                        _ => None,
                    };
                    (topic_label(&name(id)), picture.or(glyph))
                }
            };
            cells.push(
                topic_tab(
                    id,
                    label,
                    picture,
                    shape,
                    current,
                    activity,
                    move |ev, window, app| click(ev, window, app),
                    move |ev, window, app| open_menu(ev, window, app),
                )
                .into_any_element(),
            );
        }
        if let Some(overflow) = plan.overflow {
            cells.push(overflow_chip(topic_bar::overflow_label(&overflow), shape));
        }
        cells
    }

    /// Whether a turn is in flight in one topic (#2762).
    ///
    /// A leaf drawing that session knows outright. The window's own chat knows
    /// for the conversation it has open. For a topic nobody is drawing there is
    /// no signal: the store keeps no per-topic unread count, so the chip stays
    /// quiet rather than inventing one.
    pub(crate) fn topic_is_busy(&self, id: Uuid, cx: &App) -> bool {
        match self.pane_states.get(&id) {
            Some(pane) => pane.read(cx).busy,
            None => self.turn.busy && self.chat_session == Some(id),
        }
    }

    /// Open the name prompt for a new topic under the open session (#2303).
    /// Move the topic row to the next side and save the choice (#2402).
    pub(crate) fn cycle_topic_edge(&mut self, cx: &mut Context<Self>) {
        self.topic_edge = self.topic_edge.next();
        if let Err(e) = Config::write_key_string("gui", "topic_tabs", self.topic_edge.as_str()) {
            logging::error(&format!("topic tabs: could not save position: {e}"));
        }
        cx.notify();
    }

    pub(crate) fn start_topic_draft(&mut self, cx: &mut Context<Self>) {
        if self.topic_draft_parent().is_none() {
            // Nothing on screen can hold a topic. Say so and drop the menu in
            // the same breath: the old guard returned with the menu already
            // gone and no overlay owning the keys, so the typed name and its
            // Enter fell through to the composer and were sent as a message
            // (#2605). A prompt that cannot create anything never opens.
            logging::debug("topic draft: no conversation here can hold topics");
            self.topic_menu = None;
            self.turn
                .live
                .notices
                .push("topics are off for this conversation".to_string());
            self.refocus_screen();
            cx.notify();
            return;
        }
        self.topic_draft_icon = None;
        self.topic_menu = None;
        self.topic_draft = Some(String::new());
        // The prompt is the field being typed into, so it takes the keyboard
        // while it is open; without this the name landed in the composer
        // behind it (#2665).
        self.refocus_overlay();
        cx.notify();
    }

    /// Create the named topic under the open session, then open it. An empty
    /// name is refused and the prompt stays open.
    pub(crate) fn submit_topic(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.topic_draft.as_ref().map(|d| d.trim().to_string()) else {
            return;
        };
        if name.is_empty() {
            self.turn
                .live
                .notices
                .push("a topic needs a name".to_string());
            cx.notify();
            return;
        }
        // A rename prompt renames the topic in place and changes nothing else.
        if let Some(target) = self.topic_rename {
            let db_path = self.db_path.clone();
            // The icon rides along with the rename (#2791): the draft was
            // seeded from the stored value, so only a real change writes.
            let stored_icon = self.sessions.topic_icons.get(&target).cloned();
            let draft_icon = self.topic_draft_icon.clone();
            cx.spawn(async move |this, cx| {
                match sessions::rename_session(db_path.clone(), target, name).await {
                    Ok(()) => {
                        let icon_err = if sessions::icon_write_needed(
                            stored_icon.as_deref(),
                            draft_icon.as_deref(),
                        ) {
                            sessions::set_topic_icon(db_path, target, draft_icon)
                                .await
                                .err()
                        } else {
                            None
                        };
                        this.update(cx, |this, cx| {
                            if let Some(e) = icon_err {
                                logging::error(&format!("rename icon {target}: {e}"));
                                this.turn.live.notices.push(format!("rename icon: {e}"));
                            }
                            this.topic_draft = None;
                            this.topic_rename = None;
                            this.topic_draft_icon = None;
                            this.refresh_rail(cx);
                            // The stored icon may have changed, so the tabs
                            // and the rail read it back with the rest of the
                            // topic state, images included (#2791).
                            this.refresh_topic_state(cx);
                            // The prompt closed, so the caret goes back to the
                            // input the rename was asked from (#2665).
                            this.rail_focused = false;
                            this.refocus_screen();
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        logging::error(&format!("rename topic {target}: {e}"));
                        this.update(cx, |this, cx| {
                            this.turn.live.notices.push(format!("rename: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                }
            })
            .detach();
            cx.notify();
            return;
        }
        let Some(parent) = self.topic_draft_parent() else {
            return;
        };
        let db_path = self.db_path.clone();
        let icon = self
            .topic_draft_icon
            .clone()
            .unwrap_or_else(|| DEFAULT_TOPIC_ICON.to_string());
        cx.spawn(async move |this, cx| {
            // A Telegram forum gets a real forum thread before anything local
            // happens (#2762). Only the Bot API can make a topic, and the
            // thread id it returns is the key the bridge looks the session up
            // by, so a row written without it is a topic the chat never sees.
            // `Ok(None)` keeps the ordinary local path; `Err` is reported,
            // never papered over with a local-only row.
            let created = match telegram_topics::create_forum_topic_if_forum(
                db_path.clone(),
                parent,
                name.clone(),
            )
            .await
            {
                Ok(Some(topic)) => Ok(topic),
                Ok(None) => sessions::create_topic(db_path.clone(), parent, name).await,
                Err(e) => Err(e),
            };
            match created {
                Ok(topic) => {
                    if let Err(e) = sessions::set_topic_icon(db_path, topic, Some(icon)).await {
                        logging::error(&format!("topic icon {topic}: {e}"));
                    }
                    this.update(cx, |this, cx| {
                        this.topic_draft = None;
                        this.topic_draft_icon = None;
                        this.refresh_rail(cx);
                        this.refresh_topic_state(cx);
                        this.open_session(topic, cx);
                    })
                    .ok();
                }
                Err(e) => {
                    logging::error(&format!("create topic under {parent}: {e}"));
                    this.update(cx, |this, cx| {
                        this.turn.live.notices.push(format!("topic: {e}"));
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Open the Group topics picker for `id`: load every other session as a
    /// candidate, drop the ones already in a topic tree, and show the multi
    /// select. The rail's own parent map is the source of truth, so a session
    /// that is already a parent or a child cannot be re-grouped by accident.
    pub(crate) fn start_group_topics(&mut self, id: Uuid, cx: &mut Context<Self>) {
        // Close the context menu the entry was clicked from.
        self.sessions.rail_menu = None;
        self.group_target = Some(id);
        self.group_candidates = Vec::new();
        self.group_selected.clear();
        self.group_cursor = 0;
        self.group_error = None;
        self.group_busy = false;
        self.group_filter.clear();
        self.refocus_screen();
        let db_path = self.db_path.clone();
        // Taken rows: topics themselves, every child (a key) and every parent
        // (a value) already in the rail's tree. A session that carries the
        // topics flag is NOT taken: grouping it folds its topics into it
        // first, which is the only way a family becomes a topic at depth one
        // (#2864). It used to be seeded from `sessions.topics`, which is why
        // those rows were missing from the list with no reason given.
        let mut taken: std::collections::HashSet<Uuid> =
            self.sessions.parents.keys().copied().collect();
        taken.extend(self.sessions.parents.values().copied());
        cx.spawn(async move |this, cx| {
            match sessions::load_group_candidates(db_path, id).await {
                Ok(rows) => {
                    this.update(cx, |this, cx| {
                        // A stale open (target changed while loading) is dropped.
                        if this.group_target != Some(id) {
                            return;
                        }
                        this.group_candidates = rows
                            .into_iter()
                            .filter(|(sid, _, _)| !taken.contains(sid))
                            .collect();
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.group_target == Some(id) {
                            this.group_error = Some(e);
                            cx.notify();
                        }
                    })
                    .ok();
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// The candidate rows the field lets through (#2864), in list order.
    ///
    /// Narrowing happens on the way out rather than by editing
    /// `group_candidates`, so the selection survives a filter change: a row
    /// checked under one search is still checked when the search is cleared.
    pub(crate) fn group_visible(&self) -> Vec<(Uuid, String, String)> {
        filter_group_candidates(&self.group_candidates, &self.group_filter)
    }

    /// Attach every checked candidate under the target, then close the picker
    /// and refresh the rail. A guard refusal stops the batch and shows its
    /// sentence instead of a raw constraint error.
    pub(crate) fn submit_group_topics(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.group_target else {
            return;
        };
        if self.group_busy {
            return;
        }
        let children: Vec<Uuid> = self
            .group_candidates
            .iter()
            .map(|(id, _, _)| *id)
            .filter(|id| self.group_selected.contains(id))
            .collect();
        if children.is_empty() {
            self.close_group_topics();
            return;
        }
        self.group_busy = true;
        self.group_error = None;
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            for child in children {
                // Fold first when the candidate carries topics of its own:
                // the attach guard refuses a session that still has live
                // children, and after the fold it has none (#2864). The fold
                // is its own transaction, so a refusal below leaves a
                // session with no topics rather than half a family.
                if let Err(e) = sessions::merge_topics(db_path.clone(), child).await {
                    this.update(cx, |this, cx| {
                        this.group_busy = false;
                        this.group_error = Some(e);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                if let Err(e) = sessions::attach_topic(db_path.clone(), child, parent).await {
                    this.update(cx, |this, cx| {
                        this.group_busy = false;
                        this.group_error = Some(e);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.close_group_topics();
                this.refresh_rail(cx);
                this.refresh_topic_state(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Dismiss the picker and hand the keyboard back to whatever opened it.
    pub(crate) fn close_group_topics(&mut self) {
        self.group_target = None;
        self.group_candidates = Vec::new();
        self.group_selected.clear();
        self.group_cursor = 0;
        self.group_error = None;
        self.group_busy = false;
        self.group_filter.clear();
        self.refocus_screen();
    }

    /// Re-read which sessions are topics and what each topic's parent is.
    pub(crate) fn refresh_topic_state(&mut self, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let parents = sessions::load_topic_parents(db_path.clone()).await;
            let closed = sessions::load_closed(db_path.clone()).await;
            let icons = sessions::load_topic_icons(db_path.clone()).await;
            let topics = sessions::load_topics(db_path).await;
            this.update(cx, |this, cx| {
                match closed {
                    Ok(ids) => this.sessions.closed = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("closed refresh: {e}")),
                }
                match parents {
                    Ok(pairs) => this.sessions.parents = pairs.into_iter().collect(),
                    Err(e) => logging::error(&format!("topic parents refresh: {e}")),
                }
                match topics {
                    Ok(ids) => this.sessions.topics = ids.into_iter().collect(),
                    Err(e) => logging::error(&format!("topics refresh: {e}")),
                }
                match icons {
                    Ok(pairs) => this.sessions.topic_icons = pairs.into_iter().collect(),
                    Err(e) => logging::error(&format!("topic icons refresh: {e}")),
                }
                // The custom images follow the stored values, so a rename
                // that writes one reaches the rail without a restart (#2790).
                this.refresh_icon_images();
                // Every split pane draws the same store, so the refresh that
                // just landed is theirs too (#2805).
                this.notify_panes(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the custom icon images the rail and tabs draw (#2790). One
    /// file read per icon, at refresh time; the paint only clones an Arc.
    /// A missing or undecodable file logs and drops out, so the row falls
    /// back to its platform mark instead of drawing a blank box.
    pub(crate) fn refresh_icon_images(&mut self) {
        let mut images = HashMap::new();
        for stored in self.sessions.topic_icons.values() {
            let Some(path) = topic_icons::stored_image_path(stored) else {
                continue;
            };
            let Some(format) = topic_icons::image_format(stored) else {
                continue;
            };
            match std::fs::read(&path) {
                Ok(bytes) => {
                    images.insert(stored.clone(), Arc::new(Image::from_bytes(format, bytes)));
                }
                Err(e) => logging::error(&format!("icon image {stored}: {e}")),
            }
        }
        self.icon_images = images;
    }

    /// The cached picture for a stored `img:` value, or None for anything
    /// else (#2790). Never reads a file: the cache is filled at refresh
    /// time and when an image is picked.
    pub(crate) fn custom_icon_picture(&self, stored: &str, size: f32) -> Option<AnyElement> {
        let image = self.icon_images.get(stored)?;
        Some(topic_icons::cached_picture(image.clone(), size))
    }

    /// The icon picker's "add image" cell: the platform's own file picker,
    /// then a validated copy into the icon store (#2790). A cancelled
    /// prompt changes nothing.
    pub(crate) fn pick_icon_image(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an icon image".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first().cloned() else {
                return;
            };
            this.update(cx, |this, cx| {
                this.attach_icon_image(path, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Validate one picked file and store it as the draft's icon (#2790).
    /// The copy gets a fresh uuid name, so two picks of the same file never
    /// fight over one slot and an old icon stays untouched until the rename
    /// is submitted. Failures land in the notices, never silent.
    pub(crate) fn attach_icon_image(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                self.turn.live.notices.push(format!("icon image: {e}"));
                cx.notify();
                return;
            }
        };
        if !topic_icons::image_pick_allowed(&path, meta.len()) {
            self.turn.live.notices.push(
                "icon image: pick a png, jpg, webp, gif, bmp, tiff or svg under 5MB".to_string(),
            );
            cx.notify();
            return;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.turn.live.notices.push(format!("icon image: {e}"));
                cx.notify();
                return;
            }
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let filename = format!("{}.{ext}", uuid::Uuid::new_v4());
        let dir = topic_icons::image_store_dir();
        if let Err(e) =
            std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(&filename), &bytes))
        {
            self.turn.live.notices.push(format!("icon image: {e}"));
            cx.notify();
            return;
        }
        let stored = topic_icons::img_store_name(&filename);
        if let Some(format) = topic_icons::image_format(&filename) {
            self.icon_images
                .insert(stored.clone(), Arc::new(Image::from_bytes(format, bytes)));
        }
        self.topic_draft_icon = Some(stored);
        cx.notify();
    }
}

/// The group picker's rows after the field is applied (#2864).
///
/// Case-insensitive, title only: the date is not something anyone types to
/// find a conversation. A blank field returns the list unchanged, so clearing
/// the field brings every candidate back rather than rebuilding it.
pub(crate) fn filter_group_candidates(
    candidates: &[(Uuid, String, String)],
    needle: &str,
) -> Vec<(Uuid, String, String)> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return candidates.to_vec();
    }
    candidates
        .iter()
        .filter(|(_, title, _)| title.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

#[cfg(test)]
mod group_filter_tests {
    use super::filter_group_candidates;
    use uuid::Uuid;

    fn rows() -> Vec<(Uuid, String, String)> {
        vec![
            (Uuid::from_u128(1), "Rust rewrite".into(), "11-01 10:00".into()),
            (Uuid::from_u128(2), "Telegram topics".into(), "11-02 11:00".into()),
            (Uuid::from_u128(3), "rustfmt sweep".into(), "11-03 12:00".into()),
        ]
    }

    /// The field narrows the list and clearing it brings everything back
    /// (#2864). It used to swallow every keystroke, so typing did nothing.
    #[test]
    fn the_field_narrows_the_candidates() {
        let narrowed = filter_group_candidates(&rows(), "rust");
        let titles: Vec<&str> = narrowed.iter().map(|(_, t, _)| t.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Rust rewrite", "rustfmt sweep"],
            "matching is case-insensitive and keeps list order"
        );

        let all = filter_group_candidates(&rows(), "");
        assert_eq!(all.len(), 3, "a blank field shows every candidate");

        let spaced = filter_group_candidates(&rows(), "  rust  ");
        assert_eq!(spaced.len(), 2, "surrounding space is not part of the name");
    }

    /// A search that matches nothing yields nothing, and the picker says so
    /// with its own empty state instead of listing rows that do not match.
    #[test]
    fn a_search_that_matches_nothing_returns_no_rows() {
        assert!(filter_group_candidates(&rows(), "zzz").is_empty());
    }
}
