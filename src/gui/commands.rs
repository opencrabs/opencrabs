//! What a submitted line beginning with `/` is, and what the window does.
//!
//! The composer used to hand every line to the agent, so `/usage` and `/theme`
//! reached the model as prose and it wrote about the ledger instead of opening
//! it (#2374). The names themselves are not decided here: they come from
//! `ui_core::commands`, the table the terminal has always answered, so the two
//! front ends cannot disagree about what a command is called.
//!
//! Three answers, and only one of them acts:
//!
//! * a command this window runs, which becomes [`GuiAction`];
//! * a real command with no screen here yet, which says so in one line rather
//!   than pretending the name is not a command (`/models` today);
//! * a name nothing answers, which says so and names the closest match.
//!
//! None of the three ever becomes a turn. That is the whole point: a command
//! the window cannot run must not be answered by a model that will happily
//! invent an answer.

use opencrabs::ui_core::commands::{self, Support};

use super::text_commands::{self, TextCommand};

/// What the window does about a command it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GuiAction {
    /// Download the latest release and report it, as the TUI does (#2406).
    Evolve,
    /// The theme modal: four cards over the conversation, a pick saves at once.
    OpenThemes,
    /// Agent, memory and credential settings, in the window's own modal.
    OpenSettings,
    /// The usage ledger.
    OpenUsage,
    /// Mission Control: the inbox, analytics, activity and schedule panels.
    OpenMissionControl,
    /// The key reference overlay.
    ToggleHelp,
    /// A fresh conversation.
    NewSession,
    /// Abort the turn in flight.
    StopTurn,
    /// The sessions rail: the window's own list of conversations.
    OpenSessions,
    /// The model picker, or a switch when the line named a pair.
    OpenModels,
    /// The skills list: every skill the terminal can invoke (#2466).
    OpenSkills,
    /// The project organizer and its sessions (#2669).
    OpenProjects,
    /// The onboarding wizard, opened at the step this command names
    /// (`/channels`, `/voice`, `/daemon`, ...). The name travels with the
    /// action so each door lands on its own step and no two share an arm.
    OpenOnboard(&'static str),
    /// Close the window.
    Quit,
    /// A command whose answer is text, or a turn the window composes itself
    /// (#2469). The variant travels with the action so one arm covers the
    /// whole family and the whitelist cannot list a name this window has no
    /// answer for.
    Text(TextCommand),
}

/// The window's reading of a submitted line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Run this and stop: the line never becomes a turn.
    Run(GuiAction),
    /// Run this with the text after the command, kept as typed. `/models
    /// openai/gpt-5` is the same action as a bare `/models`, but the pair has
    /// to travel with it or the switch has nothing to switch to.
    RunWith(GuiAction, String),
    /// A command the terminal answers and this window has no screen for.
    /// Carries the canonical name, so the line can name it back.
    Missing(&'static str),
    /// Nothing answers this name.
    Unknown {
        /// The word the user typed, slash included.
        typed: String,
        /// The closest table entry, when one is close enough to be worth
        /// offering.
        suggestion: Option<&'static str>,
    },
}

/// Read a submitted line: `None` when it is not a command at all.
///
/// This is the only door. A line that starts with `/` and is not a bare slash
/// always produces an `Outcome`, so it never falls through to the agent, and
/// plain text always produces `None`, so it is unchanged.
pub fn parse_command(input: &str) -> Option<Outcome> {
    let invocation = commands::parse(input)?;
    Some(match commands::support_gui(&invocation.name) {
        Support::Runs => match commands::lookup_gui(&invocation.name) {
            Some(cmd) => match action_for(cmd.name) {
                Some(action) => {
                    let arg = invocation.arg.trim();
                    if takes_argument(action) && !arg.is_empty() {
                        Outcome::RunWith(action, arg.to_string())
                    } else {
                        Outcome::Run(action)
                    }
                }
                // `support` said Runs and no arm answered: the two disagree.
                // Saying "unknown" is the safe read; the parity test in this
                // module fails first if it ever happens. A silent fallback to
                // another screen used to hide exactly this (#2470).
                None => Outcome::Unknown {
                    typed: invocation.name,
                    suggestion: None,
                },
            },
            // `support` said Runs, so `lookup` returning nothing would mean the
            // two disagree. Saying "unknown" is the safe read; the parity test
            // in this module fails first if it ever happens.
            None => Outcome::Unknown {
                typed: invocation.name,
                suggestion: None,
            },
        },
        Support::Missing => Outcome::Missing(
            commands::lookup_gui(&invocation.name)
                .map(|cmd| cmd.name)
                .unwrap_or("/"),
        ),
        Support::Unknown => Outcome::Unknown {
            suggestion: commands::suggest(&invocation.name),
            typed: invocation.name,
        },
    })
}

/// The window action behind a name in [`commands::GUI_SUPPORTED`].
///
/// Every name that list declares must land here, and every arm here must be in
/// that list: `every_supported_name_has_an_action` walks both directions.
fn action_for(name: &str) -> Option<GuiAction> {
    Some(match name {
        "/theme" => GuiAction::OpenThemes,
        "/settings" => GuiAction::OpenSettings,
        "/evolve" => GuiAction::Evolve,
        "/usage" => GuiAction::OpenUsage,
        "/help" => GuiAction::ToggleHelp,
        "/new" => GuiAction::NewSession,
        "/stop" => GuiAction::StopTurn,
        "/sessions" => GuiAction::OpenSessions,
        "/models" => GuiAction::OpenModels,
        "/skills" => GuiAction::OpenSkills,
        "/projects" => GuiAction::OpenProjects,
        "/mission-control" => GuiAction::OpenMissionControl,
        "/onboard" => GuiAction::OpenOnboard("/onboard"),
        // The workspace step exists in the wizard already; the window just
        // never offered the door the terminal's table declares (#2469).
        "/workspace" => GuiAction::OpenOnboard("/workspace"),
        "/channels" => GuiAction::OpenOnboard("/channels"),
        "/voice" => GuiAction::OpenOnboard("/voice"),
        "/image" => GuiAction::OpenOnboard("/image"),
        "/daemon" => GuiAction::OpenOnboard("/daemon"),
        "/brain" => GuiAction::OpenOnboard("/brain"),
        "/doctor" => GuiAction::OpenOnboard("/doctor"),
        "/exit" => GuiAction::Quit,
        other => return text_commands::classify(other).map(GuiAction::Text),
    })
}

/// Whether an action reads the text after the command. The
/// `/models provider/model` switch is the first one that does.
fn takes_argument(action: GuiAction) -> bool {
    // `/channels whatsapp` opens that channel's dialog instead of the menu, the
    // same argument the terminal reads off the full line (#271).
    matches!(
        action,
        GuiAction::OpenModels | GuiAction::OpenOnboard("/channels") | GuiAction::Text(_)
    )
}

/// The line the transcript shows for an outcome that does not act.
///
/// `Run` produces nothing: the action itself is the answer.
pub fn message(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Run(_) | Outcome::RunWith(..) => None,
        Outcome::Missing(name) => Some(format!(
            "{name} is a real command, but this window has no screen for it yet. \
             The terminal answers it today."
        )),
        Outcome::Unknown {
            typed,
            suggestion: Some(closest),
        } => Some(format!("{typed} is not a command. Did you mean {closest}?")),
        Outcome::Unknown {
            suggestion: None, ..
        } => Some("That is not a command. Type / to pick one from the list.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_not_a_command() {
        assert!(parse_command("hello there").is_none());
        assert!(parse_command("how do I use /usage").is_none());
        assert!(parse_command("").is_none());
        // Still being spelled: the popup owns this, not the dispatcher.
        assert!(parse_command("/").is_none());
    }

    #[test]
    fn supported_commands_become_actions() {
        assert_eq!(
            parse_command("/usage"),
            Some(Outcome::Run(GuiAction::OpenUsage))
        );
        assert_eq!(
            parse_command("/theme"),
            Some(Outcome::Run(GuiAction::OpenThemes))
        );
        // The alias users reach for, because the screen is called themes.
        assert_eq!(
            parse_command("/themes"),
            Some(Outcome::Run(GuiAction::OpenThemes))
        );
        assert_eq!(
            parse_command("/settings"),
            Some(Outcome::Run(GuiAction::OpenSettings))
        );
        assert_eq!(
            parse_command("/help"),
            Some(Outcome::Run(GuiAction::ToggleHelp))
        );
        assert_eq!(
            parse_command("  /new  "),
            Some(Outcome::Run(GuiAction::NewSession))
        );
        assert_eq!(
            parse_command("/stop"),
            Some(Outcome::Run(GuiAction::StopTurn))
        );
        assert_eq!(
            parse_command("/sessions"),
            Some(Outcome::Run(GuiAction::OpenSessions))
        );
        assert_eq!(
            parse_command("/models"),
            Some(Outcome::Run(GuiAction::OpenModels))
        );
        assert_eq!(parse_command("/exit"), Some(Outcome::Run(GuiAction::Quit)));
    }

    #[test]
    fn the_onboard_doors_open_the_step_they_name() {
        // Each name is its own action carrying its own name: the wizard opens
        // on `/channels` for /channels, not on the first step.
        for name in [
            "/onboard",
            "/channels",
            "/voice",
            "/image",
            "/daemon",
            "/brain",
            "/doctor",
        ] {
            assert_eq!(
                parse_command(name),
                Some(Outcome::Run(GuiAction::OpenOnboard(name))),
                "{name}"
            );
            assert_eq!(message(&parse_command(name).expect("a command")), None);
        }
    }

    #[test]
    fn models_keeps_the_pair_it_was_given() {
        // The switch needs the pair: dropping the argument would open a
        // picker and silently ignore what the user asked for.
        assert_eq!(
            parse_command("/models openai/gpt-5"),
            Some(Outcome::RunWith(
                GuiAction::OpenModels,
                "openai/gpt-5".to_string()
            ))
        );
        assert_eq!(
            parse_command("  /models   openai/gpt-5  "),
            Some(Outcome::RunWith(
                GuiAction::OpenModels,
                "openai/gpt-5".to_string()
            ))
        );
        // A bare name still opens the picker.
        assert_eq!(
            parse_command("/models   "),
            Some(Outcome::Run(GuiAction::OpenModels))
        );
        assert_eq!(
            message(&parse_command("/models x/y").expect("a command")),
            None
        );
    }

    #[test]
    fn mission_control_opens_the_window_screen() {
        assert_eq!(
            parse_command("/mission-control"),
            Some(Outcome::Run(GuiAction::OpenMissionControl))
        );
    }

    #[test]
    fn no_command_is_left_without_a_screen() {
        // Every name the terminal offers answers on the window (#2469,
        // #2470). The `Missing` arm stays for the day a new command lands in
        // SLASH_COMMANDS before its window wiring does, so its line is still
        // checked here: a real command must never be handed to the model.
        for command in opencrabs::ui_core::commands::gui_commands() {
            let name = command.name;
            let outcome = parse_command(name).expect("a real command");
            assert!(
                !matches!(outcome, Outcome::Missing(_)),
                "{name} is listed but answers nothing on the window"
            );
        }

        let line = message(&Outcome::Missing("/unwired")).expect("a line");
        assert!(line.contains("/unwired"), "{line}");
    }

    #[test]
    fn an_unknown_name_names_the_closest_match() {
        let outcome = parse_command("/them").expect("a command");
        assert_eq!(
            outcome,
            Outcome::Unknown {
                typed: "/them".to_string(),
                suggestion: Some("/theme")
            }
        );
        let line = message(&outcome).expect("a line");
        assert!(line.contains("/theme"), "{line}");

        // Nothing close: still a line, never silence and never a turn.
        let nothing = parse_command("/zzz").expect("a command");
        assert!(message(&nothing).is_some());
    }

    #[test]
    fn arguments_do_not_change_which_command_it_is() {
        for line in ["/usage today", "/usage  today", "/usage today  "] {
            assert_eq!(
                parse_command(line),
                Some(Outcome::Run(GuiAction::OpenUsage)),
                "{line}"
            );
        }
    }

    #[test]
    fn running_commands_say_nothing() {
        assert_eq!(message(&Outcome::Run(GuiAction::OpenThemes)), None);
    }

    #[test]
    fn every_supported_name_has_an_action() {
        // Both directions: a name listed as supported but with no arm would
        // fall into the catch-all, and an arm with no name would be dead.
        let mut seen = std::collections::HashSet::new();
        for name in commands::GUI_SUPPORTED {
            let outcome = parse_command(name).expect("a supported name is a command");
            match outcome {
                Outcome::Run(action) => {
                    seen.insert(format!("{action:?}"));
                }
                other => panic!("{name} did not run in the window: {other:?}"),
            }
        }
        assert_eq!(
            seen.len(),
            commands::GUI_SUPPORTED.len(),
            "two names share one action: {:?}",
            commands::GUI_SUPPORTED
        );
    }

    /// A stable name for each action, matched with no wildcard arm.
    ///
    /// Adding a variant to [`GuiAction`] fails to compile here until it is
    /// named, which is the point: `/evolve` shipped an implemented arm that
    /// [`commands::GUI_SUPPORTED`] never listed, so the palette badged it
    /// "terminal only" while the window could already run it (#2470).
    fn action_name(action: &GuiAction) -> &'static str {
        match action {
            GuiAction::Evolve => "Evolve",
            GuiAction::OpenThemes => "OpenThemes",
            GuiAction::OpenSettings => "OpenSettings",
            GuiAction::OpenUsage => "OpenUsage",
            GuiAction::OpenMissionControl => "OpenMissionControl",
            GuiAction::ToggleHelp => "ToggleHelp",
            GuiAction::NewSession => "NewSession",
            GuiAction::StopTurn => "StopTurn",
            GuiAction::OpenSessions => "OpenSessions",
            GuiAction::OpenModels => "OpenModels",
            GuiAction::OpenSkills => "OpenSkills",
            GuiAction::OpenProjects => "OpenProjects",
            GuiAction::OpenOnboard(_) => "OpenOnboard",
            GuiAction::Quit => "Quit",
            GuiAction::Text(_) => "Text",
        }
    }

    /// Every action the window implements.
    ///
    /// Kept beside [`action_name`]: the exhaustive match forces a new variant
    /// to be named, and this list forces it to be reachable. A name here that
    /// no whitelisted command reaches is a dead arm; an action reached by the
    /// whitelist but missing here is an unlisted feature.
    const IMPLEMENTED: &[&str] = &[
        "Evolve",
        "OpenThemes",
        "OpenSettings",
        "OpenUsage",
        "OpenMissionControl",
        "ToggleHelp",
        "NewSession",
        "StopTurn",
        "OpenSessions",
        "OpenModels",
        "OpenSkills",
        "OpenProjects",
        "OpenOnboard",
        "Quit",
        "Text",
    ];

    /// #2469: the text commands answer through `GuiAction::Text`, so the
    /// whitelist, the dispatcher and this module's own table must name the
    /// same set. A name added to one and not the others is exactly the drift
    /// that made the window answer "terminal only" for a command it could run.
    #[test]
    fn every_text_command_is_whitelisted_and_dispatches_to_text() {
        for name in text_commands::NAMES {
            assert!(
                commands::GUI_SUPPORTED.contains(name),
                "{name} is answered by the window but missing from GUI_SUPPORTED"
            );
            match parse_command(name) {
                Some(Outcome::Run(GuiAction::Text(_)))
                | Some(Outcome::RunWith(GuiAction::Text(_), _)) => {}
                other => panic!("{name} did not dispatch to Text: {other:?}"),
            }
        }
    }

    #[test]
    fn every_action_reachable_from_a_whitelisted_name() {
        let reachable: std::collections::HashSet<&'static str> = commands::GUI_SUPPORTED
            .iter()
            .filter_map(|name| match parse_command(name) {
                Some(Outcome::Run(action)) => Some(action_name(&action)),
                Some(Outcome::RunWith(action, _)) => Some(action_name(&action)),
                _ => None,
            })
            .collect();

        let implemented: std::collections::HashSet<&'static str> =
            IMPLEMENTED.iter().copied().collect();

        let dead: Vec<&&str> = implemented.difference(&reachable).collect();
        assert!(
            dead.is_empty(),
            "implemented but unreachable from GUI_SUPPORTED: {dead:?}"
        );

        let unlisted: Vec<&&str> = reachable.difference(&implemented).collect();
        assert!(
            unlisted.is_empty(),
            "reachable but missing from IMPLEMENTED: {unlisted:?}"
        );
    }
}
