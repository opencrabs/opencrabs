//! The finder: the palette the window opens, the rows it lists, the keys
//! it answers and what a pick does to the screen behind it.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// Open the Seeker card (`alt+space`, `cmd-k`): one field over the
    /// window that finds a conversation, a command or a surface with the same
    /// scoring the terminal uses (#2458).
    ///
    /// It opens only when nothing else owns the keyboard, so two overlays
    /// never stack and the card always has something to hand focus back to.
    pub(crate) fn open_finder(&mut self, cx: &mut Context<Self>) {
        if !self.open_overlays().is_empty() {
            return;
        }
        self.finder = Some(PaletteState::new(self.finder_items()));
        self.finder_channels = configured_channels();
        self.finder_selecting = false;
        self.finder_activity_reset();
        // The caret blinks on a timer, and the loop dies with the card (#2491).
        // It only toggles while the field is fresh; past CARET_IDLE_AFTER the
        // caret stays off until the next key restarts it (#2531).
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(530))
                    .await;
                let open = this
                    .update(cx, |view, cx| {
                        if view.finder.is_none() {
                            return false;
                        }
                        if opencrabs::ui_core::palette::caret_blinks(
                            view.finder_activity,
                            std::time::Instant::now(),
                            CARET_IDLE_AFTER,
                        ) {
                            view.finder_caret = !view.finder_caret;
                        } else {
                            view.finder_caret = false;
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
        logging::info("finder: opened");
        cx.notify();
    }

    /// A key reached the Seeker field: the caret comes back on and its idle
    /// clock restarts, so a paused field hides it and typing re-arms it
    /// (#2531).
    pub(crate) fn finder_activity_reset(&mut self) {
        self.finder_activity = std::time::Instant::now();
        self.finder_caret = true;
    }

    /// Close the card and hand the keyboard back to what was under it.
    pub(crate) fn close_finder(&mut self, cx: &mut Context<Self>) {
        self.finder = None;
        self.finder_selecting = false;
        self.refocus_screen();
        cx.notify();
    }

    /// The rows the card can find, built from what this window already holds:
    /// the shared command table, the destinations both clients can open, and
    /// the conversation rail.
    pub(crate) fn finder_items(&self) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = slash::SLASH_COMMANDS
            .iter()
            // A destination prints its own command row: listing both would
            // show `/usage` twice (#2491).
            .filter(|cmd| !find::is_surface_command(cmd.name))
            .map(|cmd| {
                PaletteItem::new(
                    cmd.name,
                    cmd.description,
                    PaletteKind::Command,
                    PaletteAction::RunSlash(cmd.name),
                )
                .with_keywords(find::keywords_for(cmd.name))
            })
            .collect();
        items.extend(find::surface_items());
        items.extend(help_keys::seeker_items(help_keys::HelpClient::Gui));
        // The docs pages: a query that is not a command, a screen or a
        // conversation is usually a question the documentation answers (#2461).
        items.extend(docs::items());
        // The channel setup rows: the word that names a channel has to find the
        // row that starts its setup, not only the page that describes it (#2462).
        items.extend(find::channel_items());
        // Standalone actions share one catalog; execution stays in this window.
        items.extend(actions::items());
        // The window-only rows: grouping sessions into topics is a window
        // modal, so that row prints here and nowhere else (#2713). The split
        // rows live in the shared catalog because both surfaces own panes.
        items.extend(actions::gui_items());
        for skill in opencrabs::brain::skills::load_all_skills() {
            items.push(
                PaletteItem::new(
                    skill.slash_name.clone(),
                    skill.description.clone(),
                    PaletteKind::Skill,
                    PaletteAction::RunSkill(skill.slash_name.clone()),
                )
                .with_keywords("skill workflow recipe")
                .with_description(&skill.description),
            );
        }
        // Deep search (#2721): when the query names message content, the
        // conversation row carries the matching snippet in place of the date
        // and the query as a keyword, so a session whose title never mentions
        // the word still surfaces. With no content hit the row is unchanged.
        let seeker_query = self
            .finder
            .as_ref()
            .map(|state| state.query().trim().to_string())
            .unwrap_or_default();
        let snippets: HashMap<Uuid, String> = self
            .seeker_find
            .get(&seeker_query)
            .map(|rows| {
                rows.iter()
                    .map(|row| (row.session_id, row.snippet.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for session in &self.sessions.items {
            let title = session
                .title
                .clone()
                .unwrap_or_else(|| "Untitled conversation".to_string());
            let item = match snippets.get(&session.id) {
                Some(snippet) if !seeker_query.is_empty() => PaletteItem::new(
                    title,
                    snippet.clone(),
                    PaletteKind::Session,
                    PaletteAction::OpenSession(session.id.as_u128()),
                )
                .with_keywords(&seeker_query),
                _ => PaletteItem::new(
                    title,
                    session.updated_at.format("%Y-%m-%d %H:%M").to_string(),
                    PaletteKind::Session,
                    PaletteAction::OpenSession(session.id.as_u128()),
                ),
            };
            items.push(item);
        }
        items
    }

    /// Every key while the card is up belongs to it: typing narrows the list,
    /// the arrows move the armed row and Enter runs it. Nothing falls through
    /// to the composer, which is the whole point of the card (#2458).
    pub(crate) fn finder_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        // Any key is activity: the caret comes back and its idle clock
        // restarts before the key does its work (#2531).
        self.finder_activity_reset();
        if let Some(edit) = find::editing_chord(
            &ks.key,
            ks.modifiers.control,
            ks.modifiers.platform,
            ks.modifiers.alt,
            ks.modifiers.shift,
        ) {
            self.apply_finder_edit(edit, cx);
            cx.notify();
            return;
        }
        let Some(state) = self.finder.as_mut() else {
            return;
        };
        match ks.key.as_str() {
            "escape" => {
                self.close_finder(cx);
                return;
            }
            "enter" => {
                self.run_finder_pick(cx);
                return;
            }
            "up" => state.move_by(-1, find::SEEKER_PAGE),
            "down" => state.move_by(1, find::SEEKER_PAGE),
            "pageup" => state.move_by(-(find::SEEKER_PAGE as i32), find::SEEKER_PAGE),
            "pagedown" => state.move_by(find::SEEKER_PAGE as i32, find::SEEKER_PAGE),
            "space" => state.type_char(' '),
            _ => {
                // An editing chord returned above. Any other modified key is
                // still a chord, never text to append to the query.
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    return;
                }
                if let Some(text) = ks.key_char.as_deref() {
                    for ch in text.chars() {
                        state.type_char(ch);
                    }
                }
            }
        }
        self.seeker_find_if_changed(cx);
        cx.notify();
    }

    pub(crate) fn apply_finder_edit(&mut self, edit: find::SeekerEdit, cx: &mut Context<Self>) {
        let Some(state) = self.finder.as_mut() else {
            return;
        };
        match edit {
            find::SeekerEdit::Move {
                delta,
                word,
                select,
            } => match (word, select) {
                (true, true) => state.select_word_to(delta),
                (true, false) => state.move_caret_word(delta),
                (false, true) => {
                    let len = state.query().chars().count();
                    let target = if delta < 0 {
                        state.caret().saturating_sub(delta.unsigned_abs())
                    } else {
                        state.caret().saturating_add(delta.unsigned_abs()).min(len)
                    };
                    state.select_to(target);
                }
                (false, false) => state.move_caret(delta),
            },
            find::SeekerEdit::Home { select } => {
                if select {
                    state.select_to(0);
                } else {
                    state.move_caret_to(0);
                }
            }
            find::SeekerEdit::End { select } => {
                let end = state.query().chars().count();
                if select {
                    state.select_to(end);
                } else {
                    state.move_caret_to(end);
                }
            }
            find::SeekerEdit::Backspace { word: true } => state.delete_word_back(),
            find::SeekerEdit::Backspace { word: false } => state.backspace(),
            find::SeekerEdit::Delete { word: true } => state.delete_word_forward(),
            find::SeekerEdit::Delete { word: false } => state.delete_forward(),
            find::SeekerEdit::SelectAll => state.select_all(),
            find::SeekerEdit::Copy => {
                if let Some(text) = state.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                }
            }
            find::SeekerEdit::Cut => {
                if let Some(text) = state.take_selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            find::SeekerEdit::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    state.insert_text(&text);
                }
            }
        }
        self.seeker_find_if_changed(cx);
    }

    /// Translate a window point to the Seeker's character caret through the
    /// shaped line captured during paint, exactly as the composer does.
    pub(crate) fn seeker_index_for_position(&self, position: Point<Pixels>, cx: &App) -> usize {
        let Some(state) = self.finder.as_ref() else {
            return 0;
        };
        let paint = self.finder_paint.read(cx);
        let (Some(line), Some(bounds)) = (paint.line.as_ref(), paint.bounds.as_ref()) else {
            return 0;
        };
        let relative = point(position.x - bounds.left(), position.y - bounds.top());
        let byte = line
            .closest_index_for_position(relative, paint.line_height)
            .unwrap_or_else(|index| index)
            .min(state.query().len());
        find::byte_to_char(state.query(), byte)
    }

    pub(crate) fn finder_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let index = self.seeker_index_for_position(event.position, cx);
        self.finder_selecting = true;
        self.finder_activity_reset();
        if let Some(state) = self.finder.as_mut() {
            if event.modifiers.shift {
                state.select_to(index);
            } else {
                state.move_caret_to(index);
            }
        }
        cx.notify();
    }

    pub(crate) fn finder_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.finder_selecting {
            return;
        }
        let index = self.seeker_index_for_position(event.position, cx);
        self.finder_activity_reset();
        if let Some(state) = self.finder.as_mut() {
            state.select_to(index);
        }
        cx.notify();
    }

    pub(crate) fn finder_mouse_up(&mut self, _cx: &mut Context<Self>) {
        self.finder_selecting = false;
    }

    /// Run the row Enter is on, or the best match when no arrow was pressed.
    pub(crate) fn run_finder_pick(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.finder.as_ref().and_then(|state| state.selected()) else {
            self.close_finder(cx);
            return;
        };
        self.finder = None;
        self.finder_selecting = false;
        self.refocus_screen();
        match row.item.action {
            PaletteAction::RunSlash(name) => self.send_message(name.to_string(), cx),
            // A Seeker row is a pick: while a split waits it fills the pane,
            // the way a rail row does (#2760).
            PaletteAction::OpenSession(id) => self.pick_or_open(Uuid::from_u128(id), cx),
            PaletteAction::OpenSurface(surface) => self.open_surface(surface, cx),
            PaletteAction::OpenSidebar => self.set_rail_open(true, cx),
            PaletteAction::CloseSidebar => self.set_rail_open(false, cx),
            PaletteAction::SwitchModel { provider, model } => {
                self.send_message(format!("/models {provider}/{model}"), cx)
            }
            PaletteAction::RunSkill(name) | PaletteAction::RunUserCommand(name) => {
                self.send_message(name, cx)
            }
            PaletteAction::SetupChannel(channel) => self.send_message(
                format!(
                    "Set up {} as a channel: walk me through each step and make the config change with me.",
                    channel.name()
                ),
                cx,
            ),
            // The miss row asks in a fresh session. send_message is the
            // whole seam: with none open it queues the text and creates the
            // session itself (#2482).
            PaletteAction::Ask(text) => self.send_message(text, cx),
            PaletteAction::NewTopic => {
                // The same card re-armed: field empty, conversations only,
                // so a few letters pick the session (#2482).
                self.finder = Some(PaletteState::picking(
                    find::PickFor::NewTopic,
                    self.finder_items(),
                ));
                self.finder_channels = configured_channels();
                self.refocus_overlay();
                cx.notify();
            }
            PaletteAction::NewTopicOn(id) => {
                // The picker lists every conversation, topics included, so
                // the pick means one of three things (#2604, #2605): a topic
                // cannot hold topics, a conversation with them on opens the
                // prompt, and one that never had them needs the flag written
                // before the same prompt can open.
                let id = Uuid::from_u128(id);
                match topic_pick(
                    self.sessions.parents.contains_key(&id),
                    self.sessions.is_topic_enabled(id),
                ) {
                    TopicPick::Warn => {
                        let title = self
                            .sessions
                            .items
                            .iter()
                            .find(|s| s.id == id)
                            .and_then(|s| s.title.clone())
                            .unwrap_or_else(|| "That row".to_string());
                        self.topic_warning = Some(title);
                        cx.notify();
                    }
                    TopicPick::Draft => {
                        // The exact path the "New topic" menu item walks
                        // (#2482). The conversation opens in the pane holding
                        // the caret, and the prompt only opens when the pick
                        // landed: a duplicate says so instead of drafting
                        // under a conversation the window refused to move
                        // (#2819).
                        if self.open_picked_in_focused_pane(id, true, cx) {
                            self.start_topic_draft(cx);
                        }
                    }
                    TopicPick::EnableThenDraft => {
                        if self.open_picked_in_focused_pane(id, true, cx) {
                            self.enable_topics_then(id, |this, cx| this.start_topic_draft(cx), cx);
                        }
                    }
                }
            }
            PaletteAction::Favorite => {
                // Same card, same picker, different purpose: the pick pins
                // the conversation (#2487).
                self.finder = Some(PaletteState::picking(
                    find::PickFor::Favorite,
                    self.finder_items(),
                ));
                self.finder_channels = configured_channels();
                self.refocus_overlay();
                cx.notify();
            }
            PaletteAction::FavoriteToggle(id) => {
                // The exact path the rail's pin button walks (#2487).
                self.toggle_favorite(Uuid::from_u128(id), cx);
            }
            PaletteAction::GroupTopics => {
                // Same card, same empty field, different purpose: the pick
                // names the conversation that holds the topics (#2713).
                self.finder = Some(PaletteState::picking(
                    find::PickFor::GroupTopics,
                    self.finder_items(),
                ));
                self.finder_channels = configured_channels();
                self.refocus_overlay();
                cx.notify();
            }
            PaletteAction::GroupTopicsOn(id) => {
                // The exact path the rail row's right-click entry walks
                // (#2713).
                self.start_group_topics(Uuid::from_u128(id), cx);
            }
            PaletteAction::BeginSplit(layout) => {
                // The same door the chord and the rail row menu walk, from the
                // pane holding the caret (#2712).
                self.begin_split(layout, cx);
            }
            PaletteAction::OpenDocs(path) => {
                let url = docs::url(path);
                preview::launch(&preview::url_command(&url));
            }
        }
    }

    /// Open the destination a surface row names, through the same toggles the
    /// chords use, so a pick and a key press land in exactly the same place.
    pub(crate) fn open_surface(&mut self, surface: find::Surface, cx: &mut Context<Self>) {
        match surface {
            find::Surface::Usage => self.toggle_usage(cx),
            find::Surface::Files => self.toggle_files(cx),
            find::Surface::Themes => self.toggle_themes(cx),
            find::Surface::Models => self.open_models(None, self.focused_pane(), cx),
            find::Surface::Sessions => self.toggle_rail(cx),
            find::Surface::Help => self.toggle_help(cx),
            find::Surface::Skills => self.open_skills(cx),
            // `/profiles` is a text command now (#2469): the palette row takes
            // the same door as typing it, never the model.
            find::Surface::Profiles => {
                if let Some(outcome) = commands::parse_command("/profiles") {
                    self.run_command(outcome, self.focused_pane(), cx);
                }
            }
            find::Surface::MissionControl => self.open_mission_control(cx),
        }
    }

    /// A click on printed row `ix`: arm it, then take the same path Enter
    /// takes, so a click and a key press can never act on different rows.
    /// A kind chip pressed on the Seeker card: the list reshapes through the
    /// engine's own filter, so both frontends filter identically (#2491).
    pub(crate) fn set_finder_kind_filter(&mut self, want: Option<PaletteKind>, cx: &mut Context<Self>) {
        if let Some(state) = self.finder.as_mut() {
            state.set_kind_filter(want);
            cx.notify();
        }
    }

    /// The wheel over the results card steps the same engine window the
    /// arrows walk (#2520), so wheel and keys can never disagree.
    pub(crate) fn finder_scroll(&mut self, rows: i32, cx: &mut Context<Self>) {
        if rows == 0 {
            return;
        }
        if let Some(state) = self.finder.as_mut() {
            state.move_by(rows, find::SEEKER_PAGE);
            cx.notify();
        }
    }

    pub(crate) fn pick_finder_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.finder.as_mut() {
            state.arm(ix);
        }
        self.run_finder_pick(cx);
    }

    /// Run what the screen table named. The table decides and this applies, so
    /// the routing is unit tested and the effect stays in one place.
    pub(crate) fn apply_screen_action(&mut self, action: ScreenAction, cx: &mut Context<Self>) {
        match action {
            ScreenAction::EnterUsage | ScreenAction::LeaveUsage => self.toggle_usage(cx),
            ScreenAction::EnterFiles | ScreenAction::LeaveFiles => self.toggle_files(cx),
            ScreenAction::LeaveMissionControl => self.close_mission_control(cx),
            ScreenAction::ToggleHelp => self.toggle_help(cx),
        }
    }
}
