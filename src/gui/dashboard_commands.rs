//! Slash and text command dispatch, and the stop path they share. The
//! commands themselves live in `commands.rs`; this is the door the window
//! and the composer both come through.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

/// The note a command leaves when it is refused before it runs, or `None`
/// when it runs.
///
/// A capability is asked of the backend the window holds, not of the
/// handshake: a remote crab lacks a local-only one before its `hello`
/// lands, so `/onboard` typed by hand never runs the wizard on this machine
/// for it. An operation is asked of the handshake, and with none yet it
/// passes as it always has: the crab's own refusal arrives as a notice.
///
/// `/restart` cycles this process (`SelfUpdater`), so it asks the same
/// capability the wizard does: with a crab elsewhere it would restart the
/// window's own service instead of the crab the user is talking to.
pub(crate) fn refused(
    action: commands::GuiAction,
    backend: &crate::backend_glue::Backend,
    hello: Option<&opencrabs::backend::hello::HelloReply>,
) -> Option<String> {
    use opencrabs::backend::cap::{Cap, Need};
    let this_process = match action {
        commands::GuiAction::Text(text_commands::TextCommand::Restart) => backend.lacks(Cap::Setup),
        _ => None,
    };
    let why = this_process
        .or_else(|| {
            commands::needs(action).iter().find_map(|need| match *need {
                Need::Cap(cap) => backend.lacks(cap),
                Need::Op(_) => None,
            })
        })
        .or_else(|| commands::unavailable(action, hello))?;
    Some(format!("{}: {why}", command_name(action)))
}

/// The name a refused command is called by in its note.
fn command_name(action: commands::GuiAction) -> &'static str {
    match action {
        commands::GuiAction::OpenOnboard(name) => name,
        commands::GuiAction::Text(command) => text_commands::name(command),
        _ => slash::GUI_SUPPORTED
            .iter()
            .copied()
            .find(|name| commands::parse_command(name) == Some(commands::Outcome::Run(action)))
            .unwrap_or("this command"),
    }
}

impl Dashboard {
    /// Run a submitted command, or say why it does not run here.
    ///
    /// The reading is `commands::parse_command`, which is pure and unit
    /// tested; this only applies what it decided. Nothing in here may fall
    /// through to the agent: a command the window cannot run gets one line,
    /// because a model asked to answer `/usage` writes a paragraph about
    /// usage instead of opening the ledger (#2374).
    ///
    /// One exception, and it is not a screen command (#3198): a slash that
    /// names a skill or a user command IS a prompt, so it goes out as the
    /// turn those lists already send. `/usage` still never reaches the model.
    pub(crate) fn run_command(
        &mut self,
        outcome: commands::Outcome,
        session: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        // The pair has to travel with the action: `/models openai/gpt-5` is a
        // switch, a bare `/models` is the picker, and dropping the argument
        // would silently turn one into the other.
        let (action, arg) = match outcome {
            commands::Outcome::Run(action) => (action, None),
            commands::Outcome::RunWith(action, arg) => (action, Some(arg)),
            // Skills and user commands are prompts, not screens (#3198): the
            // palette and the skills list already run both, so the same
            // slash typed here gets the same turn. The #2374 rule holds for
            // everything else: only a name that IS a prompt reaches the
            // agent, a screen command the window cannot run keeps its line.
            commands::Outcome::Unknown { typed, suggestion } => {
                if !self.run_prompt_command(&typed, cx) {
                    let outcome = commands::Outcome::Unknown { typed, suggestion };
                    if let Some(line) = commands::message(&outcome) {
                        self.turn.live.notices.push(line);
                        cx.notify();
                    }
                }
                return;
            }
            other => {
                if let Some(line) = commands::message(&other) {
                    self.turn.live.notices.push(line);
                    cx.notify();
                }
                return;
            }
        };
        // What the palette greys out does not run when typed by hand either.
        if let Some(line) = refused(action, &self.backend, self.hello.as_ref()) {
            self.turn.live.notices.push(line);
            cx.notify();
            return;
        }
        match action {
            commands::GuiAction::OpenThemes => {
                // The toggle closes it, so ask first: `/theme` on the open
                // modal must not reopen it behind itself.
                if !self.themes_open {
                    self.toggle_themes(cx);
                }
            }
            commands::GuiAction::OpenSettings => {
                if !self.settings_open {
                    self.toggle_settings(cx);
                }
            }
            commands::GuiAction::OpenUsage => {
                if self.screen != Screen::Usage {
                    self.toggle_usage(cx);
                }
            }
            commands::GuiAction::OpenMissionControl => {
                if self.screen != Screen::MissionControl {
                    self.open_mission_control(cx);
                }
            }
            commands::GuiAction::ToggleHelp => self.toggle_help(cx),
            // `/new` follows the crab on screen: the window's backend is the
            // crab it holds, so the row is created there (#3133).
            commands::GuiAction::NewSession => self.new_session(cx),
            commands::GuiAction::StopTurn => self.stop_turn(cx),
            commands::GuiAction::OpenSessions => self.toggle_rail(cx),
            commands::GuiAction::OpenModels => self.open_models(arg, session, cx),
            commands::GuiAction::OpenSkills => self.open_skills(cx),
            commands::GuiAction::OpenProjects => self.open_projects(cx),
            commands::GuiAction::OpenTasks => {
                // Nothing running has no list to open, and a command that
                // does nothing reads as a broken one: say so, as the
                // terminal does.
                match session.filter(|id| !self.task_rows(*id).is_empty()) {
                    Some(id) => self.focus_tasks(id, cx),
                    None => {
                        self.turn
                            .live
                            .notices
                            .push("No background tasks running.".to_string());
                        cx.notify();
                    }
                }
            }
            commands::GuiAction::OpenOnboard(name) => self.open_onboard(name, arg, cx),
            commands::GuiAction::Text(command) => self.run_text_command(command, arg, session, cx),
            commands::GuiAction::Quit => cx.quit(),
            commands::GuiAction::Evolve => {
                let session = self.chat_session.unwrap_or_else(Uuid::nil);
                self.turn
                    .live
                    .notices
                    .push("Checking for a new release...".to_string());
                agent_bridge::spawn_evolve(self.backend.clone(), self.sink.clone(), session);
            }
        }
    }

    /// The commands that answer in text or compose a turn (#2469).
    ///
    /// `/compact` and `/goal` are dispatched here because they compose a turn,
    /// which only the window has. `send_command_turn` keeps that turn on the
    /// session that asked for it. Everything else runs on the bridge against
    /// the engine's own entry points, so the answer a window user reads is the
    /// one the terminal prints.
    pub(crate) fn run_text_command(
        &mut self,
        command: text_commands::TextCommand,
        arg: Option<String>,
        session: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        use text_commands::TextCommand as C;
        let arg = arg.unwrap_or_default();
        match command {
            C::Compact => {
                let pct = match (self.turn.ctx_tokens, self.ctx_max) {
                    (Some(used), max) if max > 0 => f64::from(used) / f64::from(max) * 100.0,
                    _ => 0.0,
                };
                // Send first, then report (#1375): a notice printed before the
                // trigger claims a compaction that may never start.
                self.send_command_turn(
                    session,
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string(),
                    Some("/compact".to_string()),
                    cx,
                );
                self.turn
                    .live
                    .notices
                    .push(opencrabs::tui::compact_notice::requested(pct));
                cx.notify();
            }
            C::Cowork => {
                // No screen in either frontend: the agent calls the tool that
                // builds the deep link, exactly as the terminal asks it to.
                let shown = format!("/cowork {arg}");
                self.send_command_turn(
                    session,
                    text_commands::cowork_prompt(&arg),
                    Some(shown.trim_end().to_string()),
                    cx,
                );
            }
            C::Goal => {
                if opencrabs::brain::goal::is_bare(&arg) {
                    // Bare `/goal` is denied with the usage line and never
                    // reaches the model, exactly as the terminal does.
                    self.turn
                        .live
                        .notices
                        .push(opencrabs::brain::goal::goal_usage_warning());
                    cx.notify();
                } else {
                    let shown = format!("/goal {arg}");
                    self.send_command_turn(
                        session,
                        opencrabs::brain::goal::goal_command_prompt(&arg),
                        Some(shown.trim_end().to_string()),
                        cx,
                    );
                }
            }
            C::Cd => {
                // The picker owns bare `/cd` (#2527): a screen, not a text
                // reply that also lights the thinking spinner. With a path
                // the change applies on the crab the window holds, the same
                // applier the terminal uses, with no turn and no spinner.
                if arg.is_empty() {
                    self.open_cd_picker(cx);
                } else {
                    let Some(session) = session else {
                        self.turn
                            .live
                            .notices
                            .push("No conversation is open. Type /new first.".to_string());
                        cx.notify();
                        return;
                    };
                    // The path is the crab's: `~` is its home and the folder
                    // is checked on its disk, never on this machine's. A path
                    // that is not a directory there comes back as the same
                    // "Not a directory" line.
                    let typed = std::path::PathBuf::from(&arg);
                    // The apply runs off the UI thread: its row write
                    // goes through `deadpool_sqlite`, which needs a Tokio
                    // reactor, and gpui's executor is not one. Awaiting it
                    // here aborted the window instead.
                    match agent_bridge::bridge() {
                        Some(_) => agent_bridge::spawn_apply_operator_cd(
                            self.backend.clone(),
                            session,
                            typed,
                            self.sink.clone(),
                        ),
                        None => self.push_session_notice(
                            session,
                            text_commands::cd_reply(
                                &opencrabs::brain::tools::error::expand_tilde(&arg)
                                    .display()
                                    .to_string(),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                }
            }
            other => {
                let Some(session) = session else {
                    self.turn
                        .live
                        .notices
                        .push("No conversation is open. Type /new first.".to_string());
                    cx.notify();
                    return;
                };
                agent_bridge::spawn_text_command(
                    self.backend.clone(),
                    session,
                    text_commands::name(other).to_string(),
                    arg,
                    self.sink.clone(),
                );
                cx.notify();
            }
        }
    }

    /// Stop the turn the focused split leaf is running, if there is one.
    ///
    /// `/stop` and Esc share this so the two cannot drift about which turn
    /// they stop or what they leave behind (#2614, #2657). The window's turn
    /// is a separate question: a leaf runs with `pane.busy` set while the
    /// window's own flag stays clear, so a stop that looked only at that flag
    /// walked straight past a running pane and reported nothing to do.
    ///
    /// Returns true when a pane turn was actually cancelled. `why` is the name
    /// in the log line, so a stop can still be traced to the key that did it.
    ///
    /// The leaf is resolved through the tree, not read off `pane_focus`: that
    /// id is a cache, and a closed pane or a layout pruned at boot leaves it
    /// naming a session the window no longer draws (#2620). Read raw, the stop
    /// either missed the pane the user was on or reached one that was gone.
    pub(crate) fn cancel_focused_pane_turn(&mut self, why: &str, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.split_focus() else {
            return false;
        };
        let Some(pane) = self.pane_states.get(&id).cloned() else {
            return false;
        };
        // The stop has to reach the leaf's own card, so the pane is asked to
        // clear itself rather than the window reaching into it (#2805).
        let cancelled = pane.update(cx, |pane, cx| {
            let Some(cancel) = pane.cancel.take() else {
                return false;
            };
            cancel.cancel();
            pane.busy = false;
            pane.live.thinking = false;
            // The card ends here, not when the service's event lands (#2811): the
            // user asked for the stop and the pane must stop claiming a turn is
            // running before the cancellation unwinds.
            pane.live.stop();
            pane.live.notices.push("Operation cancelled.".to_string());
            cx.notify();
            true
        });
        if !cancelled {
            return false;
        }
        // The stop names its session and what is still waiting, so the flush
        // that lands with the cancellation event can be paired to it (#2695).
        logging::info(&format!(
            "pane turn cancelled by {why}: session {id}, {} still queued",
            agent_bridge::queue_snapshot(id).len()
        ));
        cx.notify();
        true
    }

    /// `/stop`: abort the turn in flight, the same effect as Esc and the
    /// same wording. The pane stops claiming a turn is running the moment the
    /// user asks, and the service's own failure event lands afterwards.
    pub(crate) fn stop_turn(&mut self, cx: &mut Context<Self>) {
        // A stop ends the offer too (#2708): the turn that made the buttons is
        // gone, so they can only send a follow-up nobody asked for. That offer
        // belongs to the session the stop can reach, and only that one: a caret
        // on a pane leaves the chat leaf's offer alone (#2885).
        if let Some(sid) = self.split_focus().or(self.chat_session) {
            agent_bridge::options_take(sid);
        }
        // The keyboard goes with it: a ring standing on a card that just left
        // the screen would hold the keys for nothing (#3101).
        self.options_focus = None;
        // A split leaf runs its own turn, so `/stop` reaches the pane the
        // keyboard is on before it reports nothing to stop (#2614).
        if self.cancel_focused_pane_turn("/stop", cx) {
            return;
        }
        // The window's turn is the chat leaf's, so a caret on a pane leaves it
        // running: the pane stop above was the only turn this command could
        // reach (#2885).
        let cancel = if self.split_focus().is_some() {
            None
        } else {
            self.turn.cancel.take()
        };
        let Some(cancel) = cancel else {
            self.turn
                .live
                .notices
                .push("Nothing is running.".to_string());
            cx.notify();
            return;
        };
        cancel.cancel();
        self.turn.busy = false;
        self.turn.live.thinking = false;
        self.turn.live.stop();
        self.turn
            .live
            .notices
            .push("Operation cancelled.".to_string());
        logging::info("turn cancelled by /stop");
        cx.notify();
    }

    /// A slash that names a prompt runs as that prompt (#3198).
    ///
    /// A skill resolves exactly as the skills list does: `skills_view::prompt`,
    /// so a `review_gate` skill carries its reminder into the turn, with the
    /// same "Running" line. A user command from `commands.toml` resolves the
    /// way the command palette does: the slash line itself goes out and the
    /// agent answers it. Returns false when neither list knows the name, so
    /// the caller can fall back to the miss line.
    fn run_prompt_command(&mut self, typed: &str, cx: &mut Context<Self>) -> bool {
        if let Some(prompt) = skill_prompt(typed) {
            self.turn.live.notices.push(format!("Running {typed}"));
            self.send_message(prompt, cx);
            return true;
        }
        if user_command_exists(typed) {
            self.send_message(typed.to_string(), cx);
            return true;
        }
        false
    }
}

/// The prompt behind a slash that names a skill, or `None`.
///
/// Pure and disk-backed, so the parity test walks every skill the home holds
/// and the composer cannot drift from the skills list (#3198).
pub(crate) fn skill_prompt(typed: &str) -> Option<String> {
    opencrabs::brain::skills::load_all_skills()
        .into_iter()
        .find(|skill| skill.slash_name == typed)
        .map(|skill| skills_view::prompt(&skill))
}

/// Whether `commands.toml` holds a command this slash names.
///
/// Pure lookup, same registry the palette and the `/help` reference read.
pub(crate) fn user_command_exists(typed: &str) -> bool {
    let brain_path = opencrabs::brain::BrainLoader::resolve_path();
    opencrabs::brain::CommandLoader::from_brain_path(&brain_path)
        .load()
        .iter()
        .any(|command| command.name == typed)
}
