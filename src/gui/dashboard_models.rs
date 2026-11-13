//! The model picker, onboarding and the skills the window can run. Every
//! write here goes through the same brain files the TUI uses, so a pair
//! chosen in the window reads back in the terminal.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// Standalone provider selection filters both pointer and keyboard rows.
    /// Onboarding keeps its wizard-wide catalogue.
    pub(crate) fn visible_model_rows(&self) -> Vec<models::Row> {
        let mut rows = models::filtered_rows(&self.models_rows, &self.models_query);
        if self.screen == Screen::Models
            && let Some(provider) = self.models_provider.as_deref()
        {
            rows.retain(|row| row.provider == provider);
        }
        rows
    }

    /// `/models` with no argument opens the picker. With an argument it is the
    /// switch the terminal performs, and no screen opens: the reply is the
    /// answer, exactly like the terminal's.
    pub(crate) fn open_models(
        &mut self,
        arg: Option<String>,
        session: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        let target = session.or_else(|| self.focused_pane());
        self.models_session = target;
        // The catalogue is read once, here, so a frame never walks it. A pane
        // carries its own pair, so the picker marks that pair rather than the
        // window's when the command came from a split.
        let current = agent_bridge::bridge().map(|b| match target {
            Some(sid) => (
                b.service.provider_name_for_session(sid),
                b.service.provider_model_for_session(sid),
            ),
            None => (b.service.provider_name(), b.service.provider_model()),
        });
        self.reset_model_picker(current, None);
        if let Some(arg) = arg {
            self.switch_model(arg, cx);
            return;
        }
        self.models_query.clear();
        self.screen = Screen::Models;
        // The list opens on the provider answering right now, so the marked
        // row and the picker's cursor agree from the first frame.
        self.models_provider = self
            .models_rows
            .iter()
            .find(|row| row.current)
            .map(|row| row.provider.clone());
        self.models_index = self
            .visible_model_rows()
            .iter()
            .position(|row| row.current)
            .unwrap_or(0);
        self.sessions.rail_menu = None;
        self.start_models_fetch();
        self.refocus_screen();
        cx.notify();
    }

    /// Reset the one picker catalogue used by `/models` and Brain Fuel. The
    /// optional wizard selector contributes its unsaved provider settings.
    pub(crate) fn reset_model_picker(
        &mut self,
        current: Option<(String, String)>,
        edited: Option<&ProviderSelectorState>,
    ) {
        self.models_rows = models::rows_for_picker(
            current
                .as_ref()
                .map(|(provider, model)| (provider.as_str(), model.as_str())),
            edited,
        );
        let mut catalogue = ProviderSelectorState::default();
        catalogue.load_custom_names();
        let active_provider = current
            .as_ref()
            .map(|(provider, _)| provider.as_str())
            .or_else(|| edited.map(|selector| selector.current_provider().id))
            .unwrap_or_default();
        self.models_pending = opencrabs::ui_core::model_selector::discovery_requests(
            &catalogue,
            active_provider,
            edited,
        );
        self.models_fetch = None;
        self.models_fetch_lifecycle.invalidate();
        self.models_fetch_error = None;
        self.models_fetching = false;
        self.models_index = self
            .models_rows
            .iter()
            .position(|row| row.current)
            .unwrap_or(0);
    }

    /// Restart discovery after an onboarding provider or credential edit.
    pub(crate) fn refresh_onboard_models(&mut self) {
        let Some(ob) = self
            .onboard
            .as_ref()
            .filter(|ob| ob.wizard.step == onboard::OnboardingStep::ProviderAuth)
        else {
            return;
        };
        let edited = ob.wizard.ps.clone();
        let provider = if edited.is_custom() {
            edited.custom_name.clone()
        } else {
            edited.current_provider().id.to_string()
        };
        let current = Some((provider, ob.model()));
        self.reset_model_picker(current, Some(&edited));
        self.start_models_fetch();
    }

    /// Refresh every provider with a saved key, not only the active one.
    /// Requests run one at a time; generation and provider identity are both
    /// checked before a response can alter the visible catalogue.
    pub(crate) fn start_models_fetch(&mut self) {
        if self.models_fetch.is_some() {
            return;
        }
        self.models_fetch_lifecycle.invalidate();
        if let Some(request) = self.models_pending.first().cloned() {
            let generation = self.models_fetch_lifecycle.begin(&request);
            self.models_fetch = Some(model_fetch::spawn(generation, request));
            self.models_fetching = true;
        } else {
            self.models_fetching = false;
        }
    }

    /// Drain the shared model-fetch consumer. True means a visible state
    /// changed and the poll tick must repaint even when no agent event arrived.
    pub(crate) fn poll_model_fetches(&mut self) -> bool {
        let outcome = self
            .models_fetch
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some((generation, outcome)) = outcome else {
            return false;
        };
        self.models_fetch = None;
        self.models_pending
            .retain(|pending| pending.provider_index != outcome.provider_index);
        let provider_index = outcome.provider_index;
        let Some(outcome) = self
            .models_fetch_lifecycle
            .finish(generation, provider_index, outcome)
        else {
            self.models_fetching = false;
            return true;
        };
        if matches!(self.screen, Screen::Models | Screen::Onboard) {
            if let Some(error) = outcome.error {
                let error = format!("{}: {error}", outcome.provider_name);
                self.models_fetch_error = Some(match self.models_fetch_error.take() {
                    Some(previous) => format!("{previous}; {error}"),
                    None => error,
                });
            }
            if !outcome.models.is_empty() {
                let selected = models::filtered_rows(&self.models_rows, &self.models_query)
                    .get(self.models_index)
                    .map(models::Row::pair);
                let current = if let Some(ob) = self.onboard.as_ref()
                    && ob.wizard.step == onboard::OnboardingStep::ProviderAuth
                {
                    let provider = if ob.wizard.ps.is_custom() {
                        ob.wizard.ps.custom_name.clone()
                    } else {
                        ob.provider_id().to_string()
                    };
                    Some((provider, ob.model()))
                } else {
                    agent_bridge::bridge().map(|bridge| {
                        (
                            bridge.service.provider_name(),
                            bridge.service.provider_model(),
                        )
                    })
                };
                models::replace_provider(
                    &mut self.models_rows,
                    &outcome.provider_id,
                    &outcome.provider_name,
                    &outcome.models,
                    current
                        .as_ref()
                        .map(|(provider, model)| (provider.as_str(), model.as_str())),
                );
                let visible = models::filtered_rows(&self.models_rows, &self.models_query);
                self.models_index = visible
                    .iter()
                    .position(|row| Some(row.pair()) == selected)
                    .or_else(|| visible.iter().position(|row| row.current))
                    .unwrap_or_else(|| self.models_index.min(visible.len().saturating_sub(1)));
            }
        }
        self.start_models_fetch();
        true
    }

    /// `/skills` opens the list the terminal's own dialog shows: every skill
    /// the loader returns, built-ins first, from the same three places. Enter
    /// runs the one under the highlight by submitting its prompt body, which
    /// is exactly what the slash does in the TUI (#2466).
    pub(crate) fn open_skills(&mut self, cx: &mut Context<Self>) {
        self.skills_all = skills_view::rows();
        self.skills_query.clear();
        self.refresh_skills();
        self.skills_index = 0;
        self.screen = Screen::Skills;
        self.sessions.rail_menu = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Narrow the visible rows to `skills_query`, the same filter the
    /// terminal's dialog applies on every keystroke. The selection restarts at
    /// the top because the row that was highlighted may no longer exist.
    pub(crate) fn refresh_skills(&mut self) {
        self.skills_rows = skills_view::matching(&self.skills_all, &self.skills_query)
            .into_iter()
            .cloned()
            .collect();
        let last = self.skills_rows.len().saturating_sub(1);
        self.skills_index = self.skills_index.min(last);
    }

    /// Run the skill at `index`: back to the conversation, then the same
    /// prompt the terminal would have sent. `prompt`, not the raw body, so a
    /// `review_gate` skill carries its reminder into the turn.
    pub(crate) fn run_skill(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(skill) = self.skills_rows.get(index) else {
            return;
        };
        let prompt = skills_view::prompt(skill);
        let slash = skill.slash_name.clone();
        self.screen = Screen::Chat;
        self.sessions.rail_menu = None;
        self.refocus_screen();
        self.turn.live.notices.push(format!("Running {slash}"));
        self.send_message(prompt, cx);
    }

    /// The one model-picker keyboard path. Onboarding Enter selects and then
    /// falls through to the wizard's normal confirm; standalone Enter switches.
    pub(crate) fn shared_models_key(
        &mut self,
        ev: &KeyDownEvent,
        onboarding: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let visible = self.visible_model_rows();
        let last = visible.len().saturating_sub(1);
        let mut query_changed = false;
        match ev.keystroke.key.as_str() {
            "up" => self.models_index = self.models_index.saturating_sub(1),
            "down" => self.models_index = (self.models_index + 1).min(last),
            "enter" => {
                let pair = visible
                    .get(self.models_index.min(last))
                    .map(models::Row::pair)
                    .or_else(|| {
                        let provider = if onboarding {
                            self.onboard.as_ref().map(|ob| {
                                if ob.wizard.ps.is_custom() {
                                    ob.wizard.ps.custom_name.clone()
                                } else {
                                    ob.provider_id().to_string()
                                }
                            })
                        } else {
                            let target = self.models_session.or(self.chat_session);
                            agent_bridge::bridge().map(|bridge| match target {
                                Some(sid) => bridge.service.provider_name_for_session(sid),
                                None => bridge.service.provider_name(),
                            })
                        };
                        models::manual_pair(
                            &self.models_query,
                            provider.as_deref().unwrap_or_default(),
                        )
                    });
                if let Some(pair) = pair {
                    if onboarding {
                        self.select_onboard_pair(&pair);
                        return false;
                    }
                    self.switch_model(pair, cx);
                }
                return !onboarding;
            }
            "backspace" => {
                self.models_query.pop();
                self.models_index = 0;
                query_changed = true;
            }
            "escape" if !onboarding => {
                if self.models_query.is_empty() {
                    self.screen = Screen::Chat;
                    self.refocus_screen();
                } else {
                    self.models_query.clear();
                    self.models_index = 0;
                }
                return true;
            }
            "tab" | "escape" if onboarding => return false,
            _ => {
                if ev.keystroke.modifiers.control
                    || ev.keystroke.modifiers.alt
                    || ev.keystroke.modifiers.platform
                {
                    return false;
                }
                let Some(ch) = ev
                    .keystroke
                    .key_char
                    .as_ref()
                    .and_then(|s| s.chars().next())
                    .filter(|ch| !ch.is_control())
                else {
                    return false;
                };
                self.models_query.push(ch);
                self.models_index = 0;
                query_changed = true;
            }
        }
        if query_changed
            && onboarding
            && let Some(ob) = self.onboard.as_mut()
        {
            ob.wizard.ps.model_filter = self.models_query.clone();
            ob.wizard.ps.selected_model = 0;
        }
        true
    }

    pub(crate) fn select_onboard_pair(&mut self, pair: &str) -> bool {
        let Some((provider, model)) = pair.split_once('/') else {
            return false;
        };
        if self
            .onboard
            .as_mut()
            .is_some_and(|ob| ob.select_pair(provider, model))
        {
            self.models_query.clear();
            self.models_index = models::ensure_pair(&mut self.models_rows, provider, model);
            return true;
        }
        false
    }

    pub(crate) fn activate_model_pair(
        &mut self,
        pair: String,
        onboarding: bool,
        cx: &mut Context<Self>,
    ) {
        if !onboarding {
            self.switch_model(pair, cx);
            return;
        }
        let provider = pair.split_once('/').map(|(provider, _)| provider);
        let provider_changed = self.onboard.as_ref().is_some_and(|ob| {
            let current = if ob.wizard.ps.is_custom() {
                ob.wizard.ps.custom_name.as_str()
            } else {
                ob.provider_id()
            };
            Some(current) != provider
        });
        if self.select_onboard_pair(&pair) && provider_changed {
            self.refresh_onboard_models();
        }
        cx.notify();
    }

    /// The wizard owns the keyboard while its screen is open: Tab and the
    /// arrows walk rows, typed characters land in the focused field, Enter
    /// confirms the step and closes when there is nothing after it.
    pub(crate) fn onboard_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.onboard.is_none() {
            self.screen = Screen::Chat;
            cx.notify();
            return;
        }
        let m = &ev.keystroke.modifiers;
        let plain = !m.control && !m.platform && !m.alt;
        let key = ev.keystroke.key.clone();
        let typed = if plain {
            ev.keystroke
                .key_char
                .as_ref()
                .and_then(|s| s.chars().next())
                .filter(|ch| !ch.is_control())
        } else {
            None
        };
        let (
            before_provider,
            before_field,
            before_key_field,
            before_endpoint_field,
            completing_input,
        ) = {
            let ob = self.onboard.as_ref().expect("checked above");
            (
                ob.wizard.ps.selected_provider,
                ob.field,
                ob.key_field(),
                ob.custom_base_url_field(),
                key == "enter"
                    && (ob.key_field() == Some(ob.field)
                        || ob.custom_base_url_field() == Some(ob.field)),
            )
        };
        if self.onboard.as_ref().is_some_and(|ob| {
            ob.step() == onboard::OnboardingStep::ProviderAuth && ob.field == ob.search_field()
        }) && self.shared_models_key(ev, true, cx)
        {
            cx.notify();
            return;
        }
        if completing_input {
            // Enter on the key or custom endpoint completes that field, as in
            // the terminal; it does not skip the list by confirming the step.
            let ob = self.onboard.as_mut().expect("checked above");
            ob.field = ob.search_field();
            self.refresh_onboard_models();
            cx.notify();
            return;
        }
        let mut confirm = false;
        let mut refresh_models = false;
        {
            let ob = self.onboard.as_mut().expect("checked above");
            match key.as_str() {
                "tab" => ob.focus_next(),
                "enter" => {
                    // On the channel list Enter flips the row it stands on, the
                    // same as the terminal; anywhere else it confirms the step.
                    if ob.focused_channel().is_some() {
                        ob.toggle_focused_channel();
                    } else {
                        confirm = true;
                    }
                }
                "space" => {
                    // Space is the terminal's flip for every row that has one: a
                    // channel, a voice engine, an image switch, the daemon switch.
                    // On a text field it is an ordinary character and still types
                    // one there (#2432).
                    if !ob.activate_focused_row()
                        && let Some(ch) = typed
                    {
                        ob.type_char(ch);
                    }
                }
                "up" => ob.walk(-1),
                "down" => ob.walk(1),
                "backspace" => {
                    if !ob.backspace() {
                        ob.focus_prev();
                    }
                }
                _ => {
                    if let Some(ch) = typed {
                        ob.type_char(ch);
                    }
                }
            }
            let provider_changed = ob.wizard.ps.selected_provider != before_provider;
            let completed_input_with_tab = key == "tab"
                && (before_key_field == Some(before_field)
                    || before_endpoint_field == Some(before_field));
            if provider_changed || completed_input_with_tab {
                refresh_models = true;
            }
        }
        if confirm {
            match self.onboard.as_mut().map(|ob| ob.advance()) {
                Some(onboard::Advance::Done) => {
                    self.finish_onboard(cx);
                    return;
                }
                Some(onboard::Advance::GenerateBrain) => {
                    self.write_brain(cx);
                }
                Some(onboard::Advance::Blocked) => {
                    // The reason is on the wizard and the pane draws it above
                    // the footer, so the step stays put and nothing is lost.
                }
                Some(onboard::Advance::Moved)
                    if self
                        .onboard
                        .as_ref()
                        .is_some_and(|ob| ob.step() == onboard::OnboardingStep::ProviderAuth) =>
                {
                    refresh_models = true;
                }
                Some(onboard::Advance::Moved) | None => {}
            }
        }
        if refresh_models {
            self.refresh_onboard_models();
        }
        cx.notify();
    }

    /// Start the brain writer on the prose the user typed into the brain step.
    /// The step stays up until the thread reports back, so the window never
    /// claims the files exist before they do.
    pub(crate) fn write_brain(&mut self, cx: &mut Context<Self>) {
        let picked = self
            .onboard
            .as_mut()
            .and_then(|ob| ob.brain_prompt().map(|prompt| (prompt, ob.workspace())));
        let Some((prompt, workspace)) = picked else {
            return;
        };
        let Some(bridge) = agent_bridge::bridge() else {
            // No service to write with: the step still finishes, its prose is
            // handed to the writer on save instead of silently dropped.
            if let Some(ob) = self.onboard.as_mut() {
                ob.mark_brain_failed("the agent is offline".to_string());
            }
            return;
        };
        if let Some(ob) = self.onboard.as_mut() {
            ob.mark_brain_generating();
        }
        self.turn
            .live
            .notices
            .push("writing your brain files".to_string());
        agent_bridge::spawn_brain_generation(
            bridge.service.clone(),
            prompt,
            workspace,
            self.sink.clone(),
        );
        cx.notify();
    }

    /// The flow is over: write what the wizard collected and close it. A
    /// failure keeps the wizard on screen with the reason, because nothing was
    /// saved (#914).
    pub(crate) fn finish_onboard(&mut self, cx: &mut Context<Self>) {
        let finished = self
            .onboard
            .as_mut()
            .map(|ob| (ob.finish(), ob.workspace()));
        match finished {
            Some((onboard::Finish::Failed(e), _)) => {
                logging::error(&format!("onboarding save: {e}"));
                self.turn
                    .live
                    .notices
                    .push(format!("setup did not save: {e}"));
            }
            Some((onboard::Finish::Saved { brain_prompt }, workspace)) => {
                self.onboard = None;
                self.screen = Screen::Chat;
                self.refocus_screen();
                self.reload_agent(cx, Some("setup saved"));
                if let Some(prompt) = brain_prompt {
                    self.turn
                        .live
                        .notices
                        .push("generating personalized brain files".to_string());
                    if let Some(bridge) = agent_bridge::bridge() {
                        agent_bridge::spawn_brain_generation(
                            bridge.service.clone(),
                            prompt,
                            workspace,
                            self.sink.clone(),
                        );
                    }
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// Esc: one step back, or out of the wizard. Nothing is discarded that a
    /// confirmed step already wrote.
    pub(crate) fn onboard_escape(&mut self, cx: &mut Context<Self>) {
        if self
            .onboard
            .as_ref()
            .is_some_and(|ob| ob.step() == onboard::OnboardingStep::ProviderAuth)
            && !self.models_query.is_empty()
        {
            self.models_query.clear();
            self.models_index = 0;
            if let Some(ob) = self.onboard.as_mut() {
                ob.wizard.ps.model_filter.clear();
                ob.wizard.ps.selected_model = 0;
            }
            cx.notify();
            return;
        }
        let stayed = self.onboard.as_mut().map(|ob| ob.escape()).unwrap_or(false);
        if stayed {
            cx.notify();
        } else {
            self.close_onboard(cx);
        }
    }

    pub(crate) fn close_onboard(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Chat;
        self.onboard = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Bring the provider stack up on the config as it is on disk now.
    ///
    /// Saving the wizard is not enough by itself: a fresh install launches on
    /// the placeholder provider, and the brain files are the system prompt, so
    /// the stack has to be rebuilt on the settings that were just written. The
    /// terminal does the same thing with `rebuild_agent_service`.
    pub(crate) fn reload_agent(&mut self, cx: &mut Context<Self>, saved: Option<&str>) {
        let config = match Config::load() {
            Ok(config) => config,
            Err(e) => {
                self.turn
                    .live
                    .notices
                    .push(format!("saved, but the settings would not load: {e}"));
                cx.notify();
                return;
            }
        };
        self.agent_ready = false;
        if let Some(line) = saved {
            self.turn.live.notices.push(line.to_string());
        }
        agent_bridge::spawn_bootstrap(config, self.sink.clone());
        cx.notify();
    }

    /// `/onboard` and the step commands. The wizard opens on the step the
    /// command named, carrying the live config, so the screen shows what is
    /// configured today rather than a blank form.
    pub(crate) fn open_onboard(
        &mut self,
        command: &'static str,
        arg: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(step) = onboard::step_for_command(command) else {
            // Every door in `GUI_SUPPORTED` names a step; if a future edit
            // adds one without a step, say so instead of opening at random.
            self.turn
                .live
                .notices
                .push(format!("{command} has no onboarding step wired yet."));
            cx.notify();
            return;
        };
        // A bare `/onboard` is the full run, resumed where the user stopped, so
        // finishing it records the steps as done. A command that names one step
        // is a quick jump: it must not mark the run complete (#2060).
        let mut wizard = if command == "/onboard" {
            onboard::Onboard::open()
        } else {
            onboard::Onboard::open_at(step)
        };
        // `/channels whatsapp` opens that dialog, not the menu: the terminal
        // reads the same word off the line (#271). An unknown channel leaves
        // the menu up, which is what `open_channel` reports.
        if let Some(channel) = arg.as_deref() {
            wizard.open_channel(channel);
        }
        // On a machine that already answers, the provider step starts on the
        // pair the session is using instead of the first row of the catalogue
        // (#2060). A first run keeps the built-in preselection from `open_at`.
        if step != onboard::OnboardingStep::HealthCheck
            && let Some(bridge) = agent_bridge::bridge()
        {
            wizard.seed_session_pair(
                &bridge.service.provider_name(),
                &bridge.service.provider_model(),
            );
        }
        self.onboard = Some(wizard);
        if self
            .onboard
            .as_ref()
            .is_some_and(|ob| ob.step() == onboard::OnboardingStep::ProviderAuth)
            && let Some(ob) = self.onboard.as_mut()
        {
            ob.field = 0;
        }
        self.models_query.clear();
        self.screen = Screen::Onboard;
        self.refresh_onboard_models();
        self.sessions.rail_menu = None;
        self.refocus_screen();
        cx.notify();
    }

    /// Put a one-line answer in the conversation that owns it, so a pane never
    /// reads a switch receipt meant for the window chat.
    pub(crate) fn push_session_notice(
        &mut self,
        session: Uuid,
        line: String,
        cx: &mut Context<Self>,
    ) {
        if self.chat_session == Some(session) {
            self.turn.live.notices.push(line);
        } else if let Some(pane) = self.pane_states.get(&session) {
            pane.update(cx, |pane, cx| {
                pane.live.notices.push(line);
                cx.notify();
            });
        } else {
            self.turn.live.notices.push(line);
        }
    }

    /// Ask the service to switch. The answer arrives as an event on the UI
    /// thread, because a switch can do network work per candidate.
    pub(crate) fn switch_model(&mut self, arg: String, cx: &mut Context<Self>) {
        // A bare model id is allowed, the same as the terminal: the row it
        // names is resolved before the ask goes out.
        let pair = match models::find(&self.models_rows, &arg) {
            Some(row) => row.pair(),
            None => arg,
        };
        let Some(session) = self.models_session.or_else(|| self.focused_pane()) else {
            self.turn.live.notices.push(
                "No conversation is open, so there is nothing to switch. Type /new first."
                    .to_string(),
            );
            cx.notify();
            return;
        };
        // One pick, one switch. Enter can repeat faster than the provider
        // handshake, and every repeat used to persist another confirmation.
        if self.models_switching.is_some() {
            logging::debug("model switch already in flight");
            return;
        }
        let Some(bridge) = agent_bridge::bridge() else {
            self.push_session_notice(
                session,
                "The agent is still starting. Try again in a moment.".to_string(),
                cx,
            );
            cx.notify();
            return;
        };
        self.models_switching = Some(session);
        agent_bridge::spawn_switch_model(bridge.service.clone(), session, pair, self.sink.clone());
        cx.notify();
    }
}
