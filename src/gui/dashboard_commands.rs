//! Slash and text command dispatch, and the stop path they share. The
//! commands themselves live in `commands.rs`; this is the door the window
//! and the composer both come through.
//!
//! Part of the dashboard split (#2822): the same inherent impl, moved out of
//! `main.rs` so a rail change and an onboarding change stop sharing a file.

use super::*;

impl Dashboard {
    /// Run a submitted command, or say why it does not run here.
    ///
    /// The reading is `commands::parse_command`, which is pure and unit
    /// tested; this only applies what it decided. Nothing in here may fall
    /// through to the agent: a command the window cannot run gets one line,
    /// because a model asked to answer `/usage` writes a paragraph about
    /// usage instead of opening the ledger (#2374).
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
            other => {
                if let Some(line) = commands::message(&other) {
                    self.turn.live.notices.push(line);
                    cx.notify();
                }
                return;
            }
        };
        match action {
            commands::GuiAction::OpenThemes => {
                // The toggle closes it, so ask first: `/theme` on the open
                // modal must not reopen it behind itself.
                if !self.themes_open {
                    self.toggle_themes(cx);
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
            commands::GuiAction::NewSession => self.new_session(cx),
            commands::GuiAction::StopTurn => self.stop_turn(cx),
            commands::GuiAction::OpenSessions => self.toggle_rail(cx),
            commands::GuiAction::OpenModels => self.open_models(arg, session, cx),
            commands::GuiAction::OpenSkills => self.open_skills(cx),
            commands::GuiAction::OpenOnboard(name) => self.open_onboard(name, arg, cx),
            commands::GuiAction::Text(command) => self.run_text_command(command, arg, session, cx),
            commands::GuiAction::Quit => cx.quit(),
            commands::GuiAction::Evolve => {
                let session = self.transcript.session.unwrap_or_else(Uuid::nil);
                self.turn.live
                    .notices
                    .push("Checking for a new release...".to_string());
                agent_bridge::spawn_evolve(self.sink.clone(), session);
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
                self.turn.live
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
                    self.turn.live
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
                // the change applies locally on the bridge service, the same
                // applier the terminal uses, with no turn and no spinner.
                if arg.is_empty() {
                    self.open_cd_picker(cx);
                } else {
                    let Some(session) = session else {
                        self.turn.live
                            .notices
                            .push("No conversation is open. Type /new first.".to_string());
                        cx.notify();
                        return;
                    };
                    let expanded = opencrabs::brain::tools::error::expand_tilde(&arg);
                    if !expanded.is_dir() {
                        self.turn.live
                            .notices
                            .push(text_commands::cd_not_a_directory(&arg));
                        cx.notify();
                        return;
                    }
                    cx.spawn(async move |this, cx| {
                        let applied = match agent_bridge::bridge() {
                            Some(b) => b.service.apply_operator_cd(session, &expanded).await,
                            None => expanded.clone(),
                        };
                        let _ = this.update(cx, |this, cx| {
                            this.push_session_notice(
                                session,
                                text_commands::cd_reply(&applied.display().to_string()),
                                cx,
                            );
                            cx.notify();
                        });
                    })
                    .detach();
                }
            }
            other => {
                let Some(session) = session else {
                    self.turn.live
                        .notices
                        .push("No conversation is open. Type /new first.".to_string());
                    cx.notify();
                    return;
                };
                agent_bridge::spawn_text_command(
                    self.db_path.clone(),
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
    pub(crate) fn cancel_focused_pane_turn(&mut self, why: &str, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.pane_focus else {
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
            pane.live.stopped = true;
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
        // gone, so they can only send a follow-up nobody asked for.
        for sid in [self.focused_pane(), self.transcript.session]
            .into_iter()
            .flatten()
        {
            agent_bridge::options_take(sid);
        }
        // A split leaf runs its own turn, so `/stop` reaches the pane the
        // keyboard is on before it reports nothing to stop (#2614).
        if self.cancel_focused_pane_turn("/stop", cx) {
            return;
        }
        let Some(cancel) = self.turn.cancel.take() else {
            self.turn.live.notices.push("Nothing is running.".to_string());
            cx.notify();
            return;
        };
        cancel.cancel();
        self.turn.busy = false;
        self.turn.live.thinking = false;
        self.turn.live.stopped = true;
        self.turn.live.notices.push("Operation cancelled.".to_string());
        logging::info("turn cancelled by /stop");
        cx.notify();
    }
}
