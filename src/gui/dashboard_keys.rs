//! Key handling for the whole window, and the settle pass a plan's tasks run
//! through, plus the one chime a finished plan plays.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// A preview is modal: unsupported keys go nowhere instead of reaching the
    /// composer, tasks or transcript underneath it. Documents own their scroll
    /// directly and have no transcript follow flag to reconcile (#3041).
    fn preview_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        if key == "escape" {
            self.close_preview();
            self.refocus_screen();
            cx.notify();
            return;
        }
        let Some(document) = self.preview_document.as_ref() else {
            return;
        };
        let scroll = &document.scroll;
        let current = scroll.offset();
        let max = f32::from(scroll.max_offset().height);
        let reader_height = f32::from(scroll.bounds().size.height);
        let page = if reader_height > 0.0 {
            reader_height * 0.8
        } else {
            f32::from(window.viewport_size().height) * 0.6
        };
        let next = match key {
            "up" | "down" => {
                Some(page_landing(f32::from(current.y), max, 48.0, key == "up").offset)
            }
            "pageup" | "pagedown" => {
                Some(page_landing(f32::from(current.y), max, page, key == "pageup").offset)
            }
            "home" => Some(0.0),
            "end" => Some(-max),
            _ => None,
        };
        if let Some(next) = next {
            scroll.set_offset(point(current.x, px(next)));
            cx.notify();
        }
    }

    pub(crate) fn on_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Any deliberate keyboard input retires pointer auto-scroll first. The
        // key still reaches its normal owner, so Esc and page keys keep their
        // existing behavior instead of being swallowed by the marker (#2964).
        self.cancel_middle_scroll(cx);
        // The visible preview gets every key before any screen-level owner.
        if self.preview.is_some() {
            self.preview_key(ev, window, cx);
            return;
        }
        // The editor dialog owns every key while it is up (#3163): a typed
        // character belongs to the buffer, never to the composer underneath,
        // the same law the preview and the settings modal answer to.
        if self.editor_focus.is_some() {
            self.editor_key(ev, window, cx);
            return;
        }
        // The settings modal owns every key while it is open, on every screen
        // (#2878): an armed credential edit must not leak a typed character
        // into the composer, and the rows' arrows belong to the modal alone.
        if self.settings_open {
            let ks = &ev.keystroke;
            if ks.modifiers.control && ks.key == "," {
                self.close_settings(cx);
                return;
            }
            self.settings_modal_key(ks, cx);
            return;
        }
        // The theme modal owns every key while it is open, on every screen: it
        // is the topmost surface, so nothing under it may answer a keystroke,
        // the composer's arrows and Enter included (#2619).
        if self.themes_open {
            let ks = &ev.keystroke;
            if ks.modifiers.control && ks.modifiers.shift && ks.key == "t" {
                self.close_themes(cx);
                return;
            }
            self.theme_modal_key(ks, cx);
            return;
        }
        // A pending scope-all ask owns the keyboard on the conversation
        // (#3168): the typed command landed the question in the chat, and
        // the picker's second-tap law holds here too. While it waits, every
        // key is the ask's, so a line aimed at the answer cannot leak into
        // the composer underneath.
        if self.screen == Screen::Chat && self.models_apply_confirm.is_some() {
            self.shared_models_key(ev, false, cx);
            return;
        }
        // The Seeker card owns every key while it is up, and the chord that
        // opens it sits ahead of every other reader: one field that finds a
        // conversation, a command or a surface is only useful if the composer
        // underneath cannot eat the characters (#2458). alt+space is the
        // chord every platform can send and every finger can reach; cmd-k
        // stays as the window's own alias (#2491).
        {
            let ks = &ev.keystroke;
            let chord = find::opens(ks.modifiers.alt, ks.modifiers.platform, &ks.key);
            if self.finder.is_some() {
                self.finder_key(ks, cx);
                return;
            }
            if chord && self.screen == Screen::Chat {
                self.open_finder(cx);
                return;
            }
        }
        // The task list holds the keyboard after Down from the composer: the
        // arrows walk its rows and Esc stops the one they stand on. It reads
        // ahead of the turn's own Esc on purpose, so a press aimed at one
        // task never stops the whole turn.
        if self.task_detail.is_some() {
            self.task_detail_key(&ev.keystroke, cx);
            return;
        }
        // The follow-up offer holds the keyboard after Up from the composer
        // (#3101): the arrows walk it, Enter sends the row the ring stands on,
        // Esc hands the keys back without sending anything. It reads here, in
        // front of the task list, because only one of the two can be armed and
        // this one is the direction the user just pressed.
        if self.options_focus.is_some() {
            self.options_key(&ev.keystroke, cx);
            return;
        }
        if let Some(session) = self.task_focus
            && self.screen == Screen::Chat
        {
            self.task_key(session, &ev.keystroke, cx);
            return;
        }
        // Split panes (#2438): the chord opens a split, and while one waits for
        // its session, N makes a new one and Esc drops the pick.
        if self.screen == Screen::Chat {
            let ks = &ev.keystroke;
            if let Some(layout) = split_chord(ks) {
                self.begin_split(layout, cx);
                return;
            }
            // Alt+Tab rotates the caret between the panes and Alt+Shift+Tab
            // the other way (#2580, #2579). It runs ahead of the composer gate
            // so the chord works with the caret in the composer, and ahead of
            // bare Tab, which keeps its own jobs.
            if let Some(rotation) = rotation_chord(ks) {
                self.cycle_pane_focus(rotation == split::PaneRotation::Clockwise, cx);
                return;
            }
            if self.split_pick.is_some() {
                match ks.key.as_str() {
                    "escape" => {
                        self.cancel_split_pick(cx);
                        return;
                    }
                    "n" if !ks.modifiers.control && !ks.modifiers.platform && !ks.modifiers.alt => {
                        self.split_new(cx);
                        return;
                    }
                    _ => {}
                }
            }
            // The forward picker owns the keyboard while it is open (#2608):
            // the destination is narrowed by typing, the arrows walk it, Enter
            // takes it and Esc backs out to the picks it came from. It sits
            // this early for the same reason the split pick does: the
            // composer underneath must not answer a key meant for the bar.
            if self.forward_pick.is_some() {
                match ks.key.as_str() {
                    "escape" => self.forward_cancel(cx),
                    "enter" => self.forward_commit(cx),
                    "backspace" => {
                        self.forward_query.pop();
                        self.forward_cursor =
                            self.forward_destinations().first().map(|(id, _)| *id);
                        cx.notify();
                    }
                    "up" | "down" => {
                        let delta = if ks.key == "down" { 1 } else { -1 };
                        self.forward_step(delta, cx);
                    }
                    _ => {
                        if !ks.modifiers.control
                            && !ks.modifiers.platform
                            && !ks.modifiers.alt
                            && let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                            && !ch.is_control()
                        {
                            self.forward_query.push(ch);
                            self.forward_cursor =
                                self.forward_destinations().first().map(|(id, _)| *id);
                            cx.notify();
                        }
                    }
                }
                return;
            }
            // Select mode (#2613): Esc leaves it, Enter forwards the picks.
            // Every other key falls through, so the window's chords keep
            // answering while the selection bar replaces the composer.
            if self.msg_select.is_active() {
                match ks.key.as_str() {
                    "escape" => {
                        self.msg_select_cancel(cx);
                        return;
                    }
                    "enter" => {
                        self.msg_forward_begin(cx);
                        return;
                    }
                    _ => {}
                }
            }
            // A row's menu closes on Esc, exactly like the rail's (#2613).
            if self.msg_menu.is_some() && ks.key == "escape" {
                self.msg_menu = None;
                self.msg_menu_row = None;
                self.msg_menu_leaf = None;
                cx.notify();
                return;
            }
            // ctrl+x closes the pane holding the caret, the same chord the
            // terminal answers (#2474). It sits ahead of the composer so the
            // key never reaches the editor as a cut.
            if ks.modifiers.control
                && !ks.modifiers.platform
                && !ks.modifiers.alt
                && ks.key == "x"
                && self
                    .panes
                    .as_ref()
                    .is_some_and(|tree| tree.leaves().len() > 1)
            {
                self.close_focused_pane(cx);
                return;
            }
        }
        // Modifiers go in the line too: a bare key name cannot tell ctrl+h
        // from a typed h, which is exactly what a help-door report needs.
        let m = &ev.keystroke.modifiers;
        let mods = ChordMods {
            control: m.control,
            shift: m.shift,
            platform: m.platform,
            alt: m.alt,
        };
        // A transcript selection survives mouse release so the keyboard can
        // copy it too (#2529). A composer selection keeps its own GPUI copy
        // action; otherwise this is the transcript's shared cmd/ctrl-C path.
        let composer_has_copy_selection = self
            .composer
            .editor
            .read(cx)
            .focus_handle
            .is_focused(window)
            && self.composer.editor.read(cx).has_selection();
        if self.screen == Screen::Chat
            && !composer_has_copy_selection
            && opencrabs::ui_core::copy_select::is_copy_chord(
                &ev.keystroke.key,
                m.control,
                m.platform,
            )
            && self.copy_drag.has_selection()
        {
            // The chord takes the release path: copy, drop the highlight, and
            // say so in the composer exactly like a mouse release (#2587).
            if self.copy_drag.finish_copy(cx) {
                self.copy_drag.announce_copied(cx);
            }
            cx.notify();
            return;
        }
        let screen_action = screen_chord(&ev.keystroke.key, mods, self.screen);
        // The screen chords are answered by one pure table and they must work
        // while typing, so they sit before the editor focus gate: the editor
        // only claims IME escapes, and these keys belong to the window.
        if let Some(action) = screen_action {
            self.apply_screen_action(action, cx);
            return;
        }
        // ctrl+o folds or unfolds the newest turn, the terminal's own chord.
        // It sits with the screen keys so it works while typing.
        if ev.keystroke.modifiers.control && ev.keystroke.key == "o" && self.screen == Screen::Chat
        {
            self.toggle_newest_turn(cx);
            return;
        }
        // ctrl+n opens a new conversation, the terminal's own chord. The row
        // is created in the ledger first, so the rail and the transcript agree
        // on which session the next turn is written to.
        if ev.keystroke.modifiers.control && ev.keystroke.key == "n" {
            self.new_session(cx);
            return;
        }
        // ctrl+s shows or hides the rail: the sidebar is the window's own
        // pane, and the chord rides the control key so it is the same on every
        // platform. ctrl+l stays as the alias the terminal's session list has
        // always answered. Like the screen keys it sits before the editor
        // gate. A hidden rail has no field to keep listening, so the filter
        // exits.
        if ev.keystroke.modifiers.control && (ev.keystroke.key == "s" || ev.keystroke.key == "l") {
            self.toggle_rail(cx);
            return;
        }
        // cmd/ctrl+1..9 jumps (#3166): the nth entry of what the user is
        // looking at, `0` being position ten. The rail holding the keyboard
        // aims the chord at the rows the rail paints; otherwise the open
        // conversation's topics win when it has any, the open session never
        // changing, and the rail's rows are the fallback when it has none.
        // It sits with the screen keys so it answers while the composer holds
        // the caret, and claims the chord even when the position lands past
        // the list's end: a jump chord is never text.
        use opencrabs::ui_core::numbered_jump as jump;
        if let Some(digit) = ev.keystroke.key.chars().next()
            && let Some(position) = jump::chord_position(
                ev.keystroke.modifiers.control,
                ev.keystroke.modifiers.platform,
                digit,
            )
        {
            let list_open = self.rail_focused && self.rail_open;
            let root = self
                .chat_session
                .map(|open| self.sessions.parents.get(&open).copied().unwrap_or(open));
            let has_topics = root.is_some_and(|root| self.sessions.has_live_topics(root));
            let scope = jump::scope_for(list_open, has_topics);
            // The topic list the bar draws before it folds its tail: `All`
            // first, then the family in ledger order, so a position past the
            // pill still resolves (#3166).
            let topic_ids: Vec<Uuid> = root
                .map(|root| {
                    let mut list = vec![root];
                    list.extend(
                        self.sessions
                            .items
                            .iter()
                            .filter(|s| self.sessions.parents.get(&s.id) == Some(&root))
                            .map(|s| s.id),
                    );
                    list
                })
                .unwrap_or_default();
            let row_ids = self.sessions.painted_row_ids();
            if let Some(target) = jump::resolve_target(scope, position, &topic_ids, &row_ids) {
                match scope {
                    jump::JumpScope::Topics => {
                        // `All` opens the root, a topic opens the topic: the
                        // same ids the bar's own tabs commit (#2480).
                        self.open_session(target, cx);
                    }
                    jump::JumpScope::Sessions => {
                        // The rail's own commit path: the row opens in the
                        // pane holding the caret and the keyboard goes back
                        // to the composer. The ring follows the jump.
                        self.sessions.cursor = Some(target);
                        self.open_picked_in_focused_pane(target, true, cx);
                    }
                }
            }
            cx.notify();
            return;
        }
        // ctrl+f is the rail's find field: it narrows the session list to
        // chats whose title (or id prefix) matches, live, with the match
        // count on the bar. Any exit clears it, so the rail never hides a
        // filter behind a closed field.
        // ctrl+shift+p pins the row the keyboard is on, or the open
        // conversation when the rail has not armed one. It rides `p` on both
        // clients, so the terminal and the window pin with the same keys
        // (#2457); alt+space is the Seeker card.
        if ev.keystroke.modifiers.control && ev.keystroke.modifiers.shift && ev.keystroke.key == "p"
        {
            if let Some(id) = self.sessions.pin_target() {
                self.toggle_favorite(id, cx);
            }
            return;
        }
        if ev.keystroke.modifiers.control && ev.keystroke.key == "f" {
            if self.sessions.filter_active {
                self.close_rail_filter(cx);
            } else {
                self.arm_rail_filter(cx);
            }
            return;
        }
        // cmd+t starts a new topic under the open session (#2303). It does
        // nothing unless that session has topics turned on.
        if ev.keystroke.modifiers.platform && ev.keystroke.key == "t" {
            self.start_topic_draft(cx);
            return;
        }
        // ctrl+shift+t opens the themes screen, where a theme is previewed and
        // kept. It sits with the screen keys so it works while typing.
        if ev.keystroke.modifiers.control && ev.keystroke.modifiers.shift && ev.keystroke.key == "t"
        {
            self.toggle_themes(cx);
            return;
        }
        // ctrl+comma opens the settings modal (#2878), the chord every editor
        // user already reaches for. It sits with the screen keys for the same
        // reason as the theme chord: it works while typing.
        if ev.keystroke.modifiers.control && ev.keystroke.key == "," {
            self.toggle_settings(cx);
            return;
        }
        // While the rail filter is open it owns the keyboard, editor focus
        // included: printable characters extend the query, backspace (plain
        // or with the command key) shortens it, Enter and Esc clear it and
        // close. Anything else is swallowed so typing can never leak into
        // the chat editor while the field says it is listening.
        // The topic info panel is read only; Esc closes it and nothing else.
        if self.topic_info.is_some() && ev.keystroke.key == "escape" {
            self.topic_info = None;
            cx.notify();
            return;
        }
        // The topic name prompt owns the keyboard while it is open (#2303):
        // Enter creates the topic, Esc cancels, and everything else is the
        // composer's own editing, because the name is a real text field
        // (#2892). A bare `String` could only append and backspace: `cmd-a`
        // selected nothing, `alt+left` walked nothing and `cmd+x` cut
        // nothing. Nothing leaks into the chat editor behind it.
        if self.topic_draft.is_some() {
            let ks = &ev.keystroke;
            match ks.key.as_str() {
                "enter" => self.submit_topic(cx),
                "escape" => {
                    self.topic_draft = None;
                    self.topic_rename = None;
                    cx.notify();
                }
                _ => {
                    if let Some(edit) = text_field::editing_chord(
                        &ks.key,
                        ks.modifiers.control,
                        ks.modifiers.platform,
                        ks.modifiers.alt,
                        ks.modifiers.shift,
                    ) {
                        self.apply_topic_edit(edit, cx);
                    } else if !(ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt) {
                        // A printable key carries its character and inserts
                        // at the caret; space arrives as its own key name, so
                        // it is spelled out rather than guessed from the char.
                        let typed = if ks.key == "space" {
                            Some(' ')
                        } else {
                            ks.key_char.as_ref().and_then(|s| s.chars().next())
                        };
                        if let Some(ch) = typed
                            && let Some(draft) = self.topic_draft.as_mut()
                            && draft.type_char(ch)
                        {
                            cx.notify();
                        }
                    }
                }
            }
            return;
        }
        // The open filter panel takes Escape first (#2787): it closes the
        // chips without touching the find query underneath.
        if self.sessions.rail_filter_open && ev.keystroke.key.as_str() == "escape" {
            self.sessions.rail_filter_open = false;
            cx.notify();
            return;
        }
        if self.sessions.filter_active {
            let ks = &ev.keystroke;
            match ks.key.as_str() {
                "escape" => self.close_rail_filter(cx),
                "enter" => {
                    // Enter opens the row the field found, the terminal's own
                    // behaviour. It used to close the search and send nothing.
                    let target = self.sessions.find_target();
                    self.close_rail_filter(cx);
                    if let Some(id) = target {
                        // The field answers the split pick the way the row
                        // underneath it does (#2760).
                        self.pick_or_open(id, cx);
                    }
                }
                "up" | "down" => {
                    // The arrows step the rail from inside the field, and the
                    // match that scrolls into view is the one Enter opens.
                    let delta = if ks.key == "down" { 1 } else { -1 };
                    if let Some(id) = self.sessions.cursor_step(self.sessions.cursor, delta) {
                        self.sessions.cursor = Some(id);
                        if let Some(ix) = self.sessions.cursor_index(id) {
                            self.rail_scroll.scroll_to_item(ix);
                        }
                    }
                    cx.notify();
                }
                "backspace" => {
                    self.sessions.filter.pop();
                    self.schedule_rail_find(cx);
                    self.normalize_cursor();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_ref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.sessions.filter.push(ch);
                        self.schedule_rail_find(cx);
                        self.normalize_cursor();
                        cx.notify();
                    }
                }
            }
            return;
        }
        // Deleting a conversation has no chord. ctrl+backspace is the editor's
        // word delete (#2313), so a window binding on it would break typing,
        // and the command key is macOS-only. The row menu's delete, behind
        // the confirm dialog, is the one path: right-click and slide reveal it.
        // Esc twice stops the running turn, the TUI's own gesture. It sits
        // before the editor gate because the editor owns the keyboard while
        // typing and would swallow it.
        if self.help_open {
            let ks = &ev.keystroke;
            let scroll_by = match ks.key.as_str() {
                "up" => Some(-48.0),
                "down" => Some(48.0),
                "pageup" => Some(-400.0),
                "pagedown" => Some(400.0),
                _ => None,
            };
            if let Some(dy) = scroll_by {
                let offset = self.help_scroll.offset();
                let max = self.help_scroll.max_offset().height;
                let y = (offset.y + px(dy)).min(px(0.0)).max(-max);
                self.help_scroll.set_offset(gpui::point(offset.x, y));
                cx.notify();
                return;
            }
            if let Some(filter) = self.help_filter.as_mut() {
                match ks.key.as_str() {
                    "backspace" => {
                        filter.pop();
                        cx.notify();
                        return;
                    }
                    _ => {
                        if let Some(ch) = ks.key_char.as_deref().and_then(|s| s.chars().next())
                            && !ch.is_control()
                            && !ks.modifiers.platform
                            && !ks.modifiers.control
                        {
                            filter.push(ch);
                            cx.notify();
                            return;
                        }
                    }
                }
            } else if ks.key == "/" && !ks.modifiers.platform && !ks.modifiers.control {
                self.help_filter = Some(String::new());
                cx.notify();
                return;
            }
        }
        // A warning that stopped a pick has one answer, so it owns the
        // keyboard while it is up (#2605): Enter or Esc closes it, and every
        // other keystroke is swallowed instead of reaching the card behind
        // it.
        if self.topic_warning.is_some() || self.pane_warning.is_some() {
            if matches!(ev.keystroke.key.as_str(), "enter" | "escape") {
                self.topic_warning = None;
                self.pane_warning = None;
                self.refocus_screen();
            }
            cx.notify();
            return;
        }
        // The Group topics picker is a multi select modal: arrows move the
        // cursor, Space toggles the row under it, Enter attaches the checked
        // rows, Esc cancels. Every other key is swallowed so nothing behind
        // the card answers while it is up.
        if self.group_target.is_some() {
            match ev.keystroke.key.as_str() {
                "up" => {
                    self.group_cursor = self.group_cursor.saturating_sub(1);
                }
                "down" => {
                    let shown = self.group_visible().len();
                    if shown > 0 {
                        self.group_cursor = (self.group_cursor + 1).min(shown - 1);
                    }
                }
                "space" => {
                    let shown = self.group_visible();
                    if let Some((id, _, _)) = shown.get(self.group_cursor) {
                        let id = *id;
                        if !self.group_selected.insert(id) {
                            self.group_selected.remove(&id);
                        }
                        // A new selection re-asks the fold question (#2864).
                        self.group_warned = false;
                    }
                }
                "backspace" => {
                    self.group_filter.pop();
                    self.group_cursor = 0;
                }
                "enter" => {
                    self.submit_group_topics(cx);
                }
                "escape" => {
                    self.close_group_topics();
                }
                _ => {
                    // A printable character narrows the list (#2864). The
                    // dialog used to swallow these, so typing in the field
                    // did nothing at all.
                    if let Some(ch) = ev
                        .keystroke
                        .key_char
                        .as_deref()
                        .and_then(|s| s.chars().next())
                        && !ch.is_control()
                        && !ev.keystroke.modifiers.control
                        && !ev.keystroke.modifiers.platform
                        && !ev.keystroke.modifiers.alt
                    {
                        self.group_filter.push(ch);
                        self.group_cursor = 0;
                    }
                }
            }
            cx.notify();
            return;
        }
        // The delete confirmation is a modal with two answers, so it owns
        // both keys while it is up: Enter confirms, Esc cancels, and every
        // other keystroke is swallowed rather than reaching anything behind
        // it (#2456).
        if let Some(id) = self.confirm_delete {
            match ev.keystroke.key.as_str() {
                "enter" => {
                    self.confirm_delete = None;
                    self.delete_session(id, cx);
                }
                "escape" => {
                    self.confirm_delete = None;
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        // The disable-topics dialog owns both keys for the same reason
        // (#2863): Enter folds the family, Esc leaves it alone, and nothing
        // behind the card answers while it is up.
        if let Some(id) = self.confirm_disable_topics {
            match ev.keystroke.key.as_str() {
                "enter" => {
                    self.confirm_disable_topics = None;
                    self.disable_topics(id, cx);
                }
                "escape" => {
                    self.confirm_disable_topics = None;
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        if ev.keystroke.key == "escape" {
            // The key reference paints above everything and costs nothing to
            // close, so it goes first: the nearest thing to the user answers.
            if self.help_open {
                // A filter typed after `/` clears first; a second Esc closes (#2407).
                if self.help_filter.take().is_none() {
                    self.help_open = false;
                }
                cx.notify();
                return;
            }
            // The context menu closes next: Esc dismisses the nearest
            // affordance, never the far one, and a dismissal writes
            // nothing (#2354).
            if self.sessions.rail_menu.take().is_some() {
                cx.notify();
                return;
            }
            // The command list lives inside the composer, so with it open Esc
            // closes the list and keeps the draft: the terminal's behaviour,
            // and the nearest thing answers first.
            // The list belongs to whichever field opened it, so Esc closes the
            // focused pane's list rather than the chat's under it (#2614).
            let owner = self.composer_entity(cx);
            let spelling = owner.read(cx).list_open();
            if spelling {
                owner.update(cx, |editor, cx| {
                    editor.dismiss_command_popup(cx);
                    editor.dismiss_emoji_popup(cx);
                });
                cx.notify();
                return;
            }
            // The log viewer covers the screen while it is open, so every key
            // including Esc belongs to it, exactly as the terminal binds it.
            if self.screen == Screen::MissionControl && self.mc_logs.is_some() {
                self.mission_control_key(ev, cx);
                return;
            }
            // An open Mission Control record is the nearest thing Esc can answer:
            // it closes the popup and leaves the screen alone.
            if self.screen == Screen::MissionControl && self.mc_nav.detail {
                self.mc_nav.detail = false;
                cx.notify();
                return;
            }
            // Projects backs out of its innermost surface first: name input,
            // assignment, detail, then the screen itself.
            if self.screen == Screen::Projects {
                self.projects_escape(cx);
                return;
            }
            // The usage ledger and the files list are full screens, so Esc
            // leaves them the same way their own chords do. It sits before
            // the turn gate: a running turn must not hold a screen open.
            if let Some(action) = screen_chord("escape", mods, self.screen) {
                self.apply_screen_action(action, cx);
                return;
            }
            // Esc on the models screen leaves it and changes nothing: the
            // picker is a list, not a commitment.
            if self.screen == Screen::Models || self.screen == Screen::Skills {
                self.screen = Screen::Chat;
                self.refocus_screen();
                cx.notify();
                return;
            }
            // Esc on the /cd picker cancels and changes nothing (#2527). It
            // sits with the screens above: a running turn must not hold it.
            if self.screen == Screen::DirectoryPicker {
                self.close_cd_picker(cx);
                return;
            }
            // Esc in the wizard goes back one step, clearing a model search
            // first, and closes the screen on the first step. The terminal's
            // own rule, so one stray key never throws away a half-filled run.
            if self.screen == Screen::Onboard {
                self.onboard_escape(cx);
                return;
            }
            // A split leaf runs its own turn with `pane.busy` set while
            // `self.turn.busy` stays clear, so the idle path below would hand the
            // keyboard back and leave that turn streaming (#2657). The stop
            // reaches the pane the keyboard is on first, the same order
            // `/stop` uses (#2614).
            if self.cancel_focused_pane_turn("Esc", cx) {
                return;
            }
            // The window's own turn is the chat leaf's, so a caret on another
            // pane must not reach it either. Same branch as idle, because from
            // where the caret is that turn is already out of sight (#2616).
            if !self.turn.busy || self.split_focus().is_some() {
                // Idle Esc hands the keyboard back. The rail's mode is the
                // only thing Esc owns here, and Tab is what arms it. The old
                // blur bridge existed only because focus never moved off the
                // composer (#2351, #2454). A turn running in another
                // conversation counts as idle here: Esc must never cancel work
                // the user cannot see (#2616).
                if self.rail_focused {
                    self.rail_focused = false;
                    self.refocus_screen();
                }
                cx.notify();
                return;
            }
            // One Esc stops the turn, and the queue becomes the next turn. The
            // old two-press window existed because nothing flushed the queue on
            // a stop, so a single press left the user watching a process they
            // had already asked to kill with their typed text stranded behind
            // it (#2631).
            //
            // A stop that beats the first token takes the message back
            // (#2635): the turn never reached the conversation, so the text
            // belongs in the composer to be corrected and sent again instead of
            // sitting in a chat that will never answer it, and the rows the
            // turn opened are deleted rather than redrawn.
            let mut returned = false;
            if let Some(sid) = self.chat_session
                && self.turn.busy
                && self.turn.live.untouched()
            {
                // The question belongs to the chat leaf, so it goes back to the
                // field that owns that session. `composer_entity` resolves the
                // caret, and with a split that would hand one conversation's
                // question to another.
                let composer = self.composer.editor.clone();
                // Only into an empty field: anything typed since the send is
                // the user's, and taking the question back must never cost it.
                // One condition, not two nested ones: the take has to run only
                // on an empty field, and clippy reads the nesting as a mistake
                // rather than the guard it is (#2641).
                if composer.read(cx).content.trim().is_empty()
                    && let Some(text) = self.turn.live.user.take()
                {
                    composer.update(cx, |editor, cx| {
                        editor.restore_draft(text, cx);
                        editor.show_returned(cx);
                    });
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_undo_last_turn(
                            bridge.service.clone(),
                            sid,
                            self.sink.clone(),
                        );
                    }
                    self.turn.live.clear();
                    returned = true;
                }
            }
            if let Some(cancel) = self.turn.cancel.take() {
                cancel.cancel();
            }
            // Same wording and immediate release as `abort_active_turn`: the
            // pane stops claiming a turn is running the moment the user asked
            // it to stop, and the service's own Failed event lands afterwards.
            self.turn.busy = false;
            self.turn.live.thinking = false;
            // Only a turn that still has something to show keeps the stop
            // (#2811): a handed-back question already cleared the card, and
            // marking that one stopped would draw an empty card with no text.
            if !returned {
                self.turn.live.stop();
            }
            self.turn.cancel = None;
            let waiting = self.queued_for_strip().len();
            if waiting > 0 {
                // The send itself waits for the cancellation to be
                // acknowledged: a second `spawn_turn` on a session whose cancel
                // is still unwinding would run two turns into one
                // conversation. The turn's own event drains the queue (see the
                // post-batch `drain_pending`), which is milliseconds after the
                // stop (#2631).
                self.turn.live.notices.push(format!(
                    "Operation cancelled. Sending the {waiting} queued."
                ));
            } else if !returned {
                // A handed-back message already says so in the composer, so
                // the notice would be the second time the user is told.
                self.turn
                    .live
                    .notices
                    .push("Operation cancelled.".to_string());
            }
            logging::info("turn cancelled by Esc");
            self.sync_composer_hint(cx);
            cx.notify();
            return;
        }
        // Page keys scroll the transcript and ctrl+k clears the session (#2430).
        // Both sit before the composer gate, so they work while the composer has focus.
        if self.screen == Screen::Chat {
            let key = ev.keystroke.key.as_str();
            if matches!(key, "pageup" | "pagedown") {
                let page = f32::from(window.viewport_size().height) * 0.8;
                let up = key == "pageup";
                // The page keys act on the pane the caret is on, so a split
                // scrolls the transcript the accent names rather than the
                // chat underneath it (#2475, #2472).
                let target = self
                    .pane_focus
                    .filter(|id| self.pane_states.contains_key(id));
                // Where the page lands, and whether that landing is the end of
                // the content, comes from the scroll module's page rule so both
                // answers use the arithmetic the wheel and the growth check
                // already share. Asking the question of the clamped offset
                // answered "yes" every time, and PageUp raised the offset only to
                // be snapped straight back down by the same handler (#2959).
                let (x, landing) = match target.and_then(|id| self.pane_states.get(&id)) {
                    Some(pane) => {
                        let pane = pane.read(cx);
                        let offset = pane.scroll.offset();
                        let max = f32::from(pane.scroll.max_offset().height);
                        (offset.x, page_landing(f32::from(offset.y), max, page, up))
                    }
                    None => {
                        let offset = self.chat_state.scroll.offset();
                        let max = f32::from(self.chat_state.scroll.max_offset().height);
                        (offset.x, page_landing(f32::from(offset.y), max, page, up))
                    }
                };
                let y = px(landing.offset);
                match target {
                    Some(id) => {
                        if let Some(pane) = self.pane_states.get(&id) {
                            pane.update(cx, |pane, cx| {
                                pane.scroll.set_offset(gpui::point(x, y));
                                cx.notify();
                            });
                        }
                    }
                    None => self.chat_state.scroll.set_offset(gpui::point(x, y)),
                }
                // A page key is the reader moving by hand, the same gesture
                // the terminal binds to its own scroll: up releases the follow
                // and down gives it back only once it lands on the end, so a
                // row that lands mid-page never drags the reader back down
                // (#2698).
                match (target, landing.at_end) {
                    (Some(id), true) => self.rearm_pane_follow(id, cx),
                    (Some(id), false) => self.release_pane_follow(id, cx),
                    (None, true) => self.rearm_transcript_follow(),
                    (None, false) => self.release_transcript_follow(),
                }
                cx.notify();
                return;
            }
            if key == "k" && ev.keystroke.modifiers.control {
                if let Some(id) = self.chat_session {
                    agent_bridge::spawn_clear_messages(self.db_path.clone(), id, self.sink.clone());
                }
                return;
            }
        }
        // Zoom with cmd or ctrl, in every mode and while the composer has focus.
        // It needs the chord, so a bare minus typed into the composer stays text.
        let chord = ev.keystroke.modifiers.platform || ev.keystroke.modifiers.control;
        if let Some(next) = zoom_step_for(&ev.keystroke.key, self.rem, chord) {
            self.rem = next;
            window.set_rem_size(px(self.rem));
            logging::debug(&format!("zoom -> {:.2}x", self.rem / BASE_REM));
            cx.notify();
            return;
        }
        // Tab moves the keyboard between the composer and the rail. It runs before
        // the composer gate, because taking the keyboard away from a focused
        // composer is the whole point (#2351, #2454). While an emoji or command
        // list is open, Tab accepts there instead (#2417).
        if ev.keystroke.key == "tab"
            && self.screen == Screen::Chat
            && self.rail_open
            && (self.rail_focused || !self.composer.editor.read(cx).list_open())
        {
            self.rail_focused = !self.rail_focused;
            if self.rail_focused {
                self.sessions.cursor = self.sessions.cursor.or(self.sessions.selected);
            }
            // The owner decides where focus lands: the root while the rail
            // holds the keyboard, the composer once it hands back.
            self.refocus_screen();
            cx.notify();
            return;
        }
        // Ctrl+C is the terminal's own ladder (#2308): scrolled up it gives
        // the bottom back, otherwise it throws the draft away and arms a three
        // second quit window, and the second press stops the turn in flight and
        // closes the window. It sits ahead of the composer gate so the key
        // answers while the caret is in the field; the interceptor in `main` is
        // what keeps the editor's copy chord from taking the press first when
        // nothing is selected.
        if ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.alt
            && ev.keystroke.key == "c"
        {
            self.ctrl_c_press(cx);
            return;
        }
        // While the editor holds focus it owns the keyboard: its actions
        // (send, newline, movement) run through the keymap, and raw keys
        // reaching this handler would double-handle Enter and friends.
        if self
            .composer
            .editor
            .read(cx)
            .focus_handle
            .is_focused(window)
        {
            return;
        }
        // Rail arrows and enter. This sits after the editor gate, so the
        // composer keeps its up, down and enter actions while it holds
        // focus, and before the chat-screen return, which is the dead end
        // arrows used to die in (#2351).
        if self.screen == Screen::Chat && self.rail_open {
            match ev.keystroke.key.as_str() {
                "up" | "down" if self.rail_focused => {
                    // The arrows walk the rail and open the row they stand
                    // on, so the chat shows it without an Enter. Focus stays
                    // on the rail: the composer is not refocused here (#2404).
                    let delta = if ev.keystroke.key == "down" { 1 } else { -1 };
                    if let Some(id) = self.sessions.cursor_step(self.sessions.cursor, delta) {
                        self.sessions.cursor = Some(id);
                        if let Some(ix) = self.sessions.cursor_index(id) {
                            self.rail_scroll.scroll_to_item(ix);
                        }
                        // During a split pick the arrows only walk the list (#2438).
                        if self.split_pick.is_none() {
                            self.open_picked_in_focused_pane(id, false, cx);
                        }
                    }
                    cx.notify();
                    return;
                }
                "enter" if self.rail_focused => {
                    if let Some(id) = self.sessions.cursor {
                        if self.split_pick.is_some() {
                            self.choose_split(id, cx);
                            cx.notify();
                            return;
                        }
                        self.open_picked_in_focused_pane(id, true, cx);
                        // Back to the composer, the place a newly opened
                        // conversation is about to be typed into.
                        self.refocus_screen();
                    }
                    cx.notify();
                    return;
                }
                "d" if ev.keystroke.modifiers.control => {
                    // ctrl+d asks the dialog, it never erases (#2352). The
                    // cursor row wins; with nothing armed the open chat is
                    // the target, which is the row the transcript shows.
                    if let Some(target) = self.sessions.delete_target() {
                        self.request_delete(target, cx);
                    }
                    cx.notify();
                    return;
                }
                // A printable character with the rail focused starts a search
                // (#2812). The field arms itself and takes the character, so
                // the list narrows as a name is typed instead of asking for
                // ctrl+f first. The letters the terminal answers (r, t, c) are
                // not spent here: the first keystroke of "rust" cannot also be
                // the rename command, so the window keeps them on the row menu
                // and the terminal keeps the bare keys.
                _ if self.rail_focused => {
                    let ks = &ev.keystroke;
                    if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                        // A chord keeps its own meaning; only bare typing
                        // opens the field.
                    } else if let Some(ch) = ks.key_char.as_deref().and_then(|s| s.chars().next())
                        && !ch.is_control()
                        && !ch.is_whitespace()
                    {
                        self.arm_rail_filter(cx);
                        self.sessions.filter.push(ch);
                        self.schedule_rail_find(cx);
                        self.normalize_cursor();
                        return;
                    }
                }
                // Every other key falls through to the handlers below. The
                // arms above are guarded, so the match still needs a wildcard
                // to be exhaustive: a keystroke the rail does not claim is not
                // an error, it is simply somebody else's.
                _ => {}
            }
        }
        // Chat owns no usage keys. Esc included: backing out of the
        // conversation into the ledger was the mixing the owner called out,
        // and Esc must never close the window (phase-0 lesson).
        if self.screen == Screen::Chat {
            return;
        }
        // The files browser is a list, not the ledger's grid: Esc (or the
        // ctrl+g toggle again) is the only key it owns, and it goes back to
        // the conversation.
        if self.screen == Screen::Files {
            if translate_key(ev) == UiKey::Esc {
                self.screen = Screen::Chat;
                cx.notify();
            }
            return;
        }
        // The model picker is a live-filtered list: arrows walk the visible
        // models (never its provider headers) and Enter switches. Esc clears
        // a filter first, then leaves, as the terminal's picker does.
        if self.screen == Screen::Models {
            self.shared_models_key(ev, false, cx);
            cx.notify();
            return;
        }
        // Projects owns its list, detail, assignment and name-input keys.
        if self.screen == Screen::Projects {
            self.projects_key(ev, cx);
            return;
        }
        // The skills screen is a list too: arrows walk it and Enter runs the
        // skill under the highlight, the same act as typing its slash.
        if self.screen == Screen::Skills {
            let last = self.skills_rows.len().saturating_sub(1);
            match ev.keystroke.key.as_str() {
                "up" => self.skills_index = self.skills_index.saturating_sub(1),
                "down" => self.skills_index = (self.skills_index + 1).min(last),
                "enter" => self.run_skill(self.skills_index.min(last), cx),
                "backspace" => {
                    self.skills_query.pop();
                    self.skills_index = 0;
                    self.refresh_skills();
                }
                _ => {
                    // Typing narrows the list, as it does in the terminal's
                    // dialog: a list of forty skills is unreadable without it.
                    if let Some(ch) = ev
                        .keystroke
                        .key_char
                        .as_ref()
                        .and_then(|s| s.chars().next())
                        && !ch.is_control()
                    {
                        self.skills_query.push(ch);
                        self.skills_index = 0;
                        self.refresh_skills();
                    }
                }
            }
            cx.notify();
            return;
        }
        // The /cd picker owns the keyboard on its screen (#2527), the same
        // bindings the terminal's dialog uses: arrows walk, Enter descends,
        // `.` flips hidden directories, Tab and Space select here, Esc
        // cancels (handled above with the screens).
        if self.screen == Screen::DirectoryPicker {
            let last = self.cd_entries.len().saturating_sub(1);
            match ev.keystroke.key.as_str() {
                "up" => self.cd_selected = self.cd_selected.saturating_sub(1),
                "down" => self.cd_selected = (self.cd_selected + 1).min(last),
                "enter" => self.cd_descend(self.cd_selected.min(last), cx),
                "tab" | "space" => self.cd_select_here(cx),
                _ => {
                    // `.` flips the hidden-directories toggle, as it does in
                    // the terminal's dialog.
                    if ev.keystroke.key_char.as_deref() == Some(".") {
                        self.cd_toggle_hidden(cx);
                    }
                }
            }
            cx.notify();
            return;
        }
        // The wizard owns the keyboard while it is open: typed characters go
        // into the focused field, Tab and the arrows walk the lists, Enter
        // confirms the step, Esc steps back (handled above).
        if self.screen == Screen::Onboard {
            self.onboard_key(ev, cx);
            return;
        }
        // Mission Control owns the keyboard on its own screen: bare keys walk
        // the panels and open the record, the terminal's own bindings.
        if self.screen == Screen::MissionControl {
            self.mission_control_key(ev, cx);
            return;
        }
        match input::decide(&mut self.dash, translate_key(ev)) {
            UsageKeyOutcome::Consumed => cx.notify(),
            UsageKeyOutcome::Period(period) => self.set_usage_period(period, cx),
            // `decide` maps Esc on the grid to Close, which is right for the
            // TUI's modal dialog and wrong for a desktop window: a stray Esc
            // must never destroy the window. The titlebar button and Cmd+Q
            // are the exits here.
            UsageKeyOutcome::Close => {
                // Esc on the usage screen means "back to the conversation".
                // With nothing to return to it is a no-op, never a window
                // close (phase-0 lesson).
                if self.chat_session.is_some() {
                    self.screen = Screen::Chat;
                    cx.notify();
                } else {
                    logging::info("ignored Close: the window closes from its titlebar button");
                }
            }
            UsageKeyOutcome::NotConsumed => {}
        }
    }

    /// Settle plan tasks finished since the last frame: pulse the rail, no
    /// sound (#2691, #3118).
    ///
    /// The terminal does this from its tick (`tui::app::state`), reading the
    /// plan file the tool writes. A gpui window has no tick, so the render
    /// path is the equivalent place: it already re-reads the strips by
    /// mtime, and this pass turns the settled tasks into one rail flash. The
    /// window strip and every pane strip share one ledger, so a plan drawn
    /// twice flashes once, and a reload of an unchanged file does nothing at
    /// all. That ledger is durable per session (#3004): a relaunch after a
    /// clean exit, a panic or a machine restart resumes where the last
    /// process stopped instead of flashing every settled task again. The one
    /// completion sound is the plan-finished chime, raised from
    /// `ProgressEvent::PlanCompleted`.
    pub(crate) fn settle_plan_tasks(&mut self, cx: &mut Context<Self>) {
        // Four passes a second, not one per frame (#2899). The pass clones
        // every plan document, resolves the settled ids in each and asks the
        // notification layer about them, and a frame can arrive as often as
        // the 5 Hz animation cadence with nothing else to read. A task that
        // settles between two passes still rings on the next one, well inside
        // the flash window it starts.
        const CHIME_PASS: std::time::Duration = std::time::Duration::from_millis(250);
        let now = std::time::Instant::now();
        if let Some(last) = self.chime_last
            && now.duration_since(last) < CHIME_PASS
        {
            return;
        }
        self.chime_last = Some(now);

        // A pane strip only re-reads its file when it is drawn, and this pass
        // runs before the split body is built. Refresh here so a background
        // pane that settled rings on this frame instead of the next redraw it
        // happens to get (#2733). Each read is mtime-gated.
        for pane in self.pane_states.values() {
            pane.update(cx, |pane, _| pane.plan.refresh());
        }

        // Every plan in this frame, each paired with the session that owns it,
        // so a settled task is recorded against the right conversation and
        // flashed from a single pass. A pre-init plan writes the nil session
        // until the hook stamps it, so the drawing row is the honest fallback.
        // Owned snapshots: a pane is an entity now (#2805), so a borrowed
        // document would keep its handle borrowed across the record. One
        // snapshot per pane per frame, and a plan is a handful of tasks.
        let mut plans: Vec<(Uuid, opencrabs::tui::plan::PlanDocument)> =
            Vec::with_capacity(1 + self.pane_states.len());

        if let Some(doc) = self.plan.document().cloned() {
            let session = if doc.session_id == Uuid::nil() {
                self.chat_session.unwrap_or(Uuid::nil())
            } else {
                doc.session_id
            };
            plans.push((session, doc));
        }
        for (pane_id, pane) in &self.pane_states {
            let Some(doc) = pane.read(cx).plan.document().cloned() else {
                continue;
            };
            let session = if doc.session_id == Uuid::nil() {
                *pane_id
            } else {
                doc.session_id
            };
            plans.push((session, doc));
        }

        // Record each plan against its own session's durable ledger (#3004).
        // The first pass after a launch folds in what a previous process
        // already flashed, so a plan that has not moved does not blink again;
        // a task that settled while the window was closed is still new, so it
        // blinks once. The ledger is the one record of what was fresh, so the
        // flash and the record cannot disagree.
        let mut settled: Vec<Uuid> = Vec::new();
        for (session, doc) in &plans {
            let fresh = self.settle_ledger.record(*session, Some(doc));
            if fresh.is_empty() || *session == Uuid::nil() {
                continue;
            }
            settled.push(*session);
        }
        if settled.is_empty() {
            return;
        }
        // One flash per settled session: the blink is the per-task signal, and
        // the plan-finished chime is the only sound.
        for session in settled {
            self.plan_flash_until.insert(
                session,
                now + opencrabs::utils::completion_sound::FLASH_WINDOW,
            );
        }
    }

    /// Whether any rail flash window is still open, dropping the expired ones.
    ///
    /// Read by the idle poll: while a bar blinks, the rail has to keep
    /// repainting so the orange alternates, and once every window closed the
    /// repaint budget goes back to zero. Pruned here rather than at paint time
    /// so the map cannot grow past the sessions that are actually flashing.
    pub(crate) fn plan_flash_active(&mut self, now: std::time::Instant) -> bool {
        self.plan_flash_until.retain(|_, until| now < *until);
        !self.plan_flash_until.is_empty()
    }

    /// The plan-finished chime, honouring the operator's opt-in and the build
    /// feature (#2691, #3118). The flag is read at the moment of the chime, so
    /// flipping `[agent] notify_on_plan_completed` in a live session takes
    /// effect without a restart, exactly as the terminal's arm does.
    #[cfg(feature = "notification-sound")]
    pub(crate) fn play_plan_chime() {
        let enabled = opencrabs::config::Config::load()
            .map(|c| c.agent.notify_on_plan_completed)
            .unwrap_or(false);
        if enabled {
            opencrabs::utils::completion_sound::play_completion_sound();
        }
    }

    #[cfg(not(feature = "notification-sound"))]
    pub(crate) fn play_plan_chime() {}
}
