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

/// What the window does about a command it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GuiAction {
    /// Download the latest release and report it, as the TUI does (#2406).
    Evolve,
    /// The themes screen: preview, keep, or restore.
    OpenThemes,
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
    /// The onboarding wizard, opened at the step this command names
    /// (`/channels`, `/voice`, `/daemon`, ...). The name travels with the
    /// action so each door lands on its own step and no two share an arm.
    OpenOnboard(&'static str),
    /// Close the window.
    Quit,
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
    Some(match commands::support(&invocation.name) {
        Support::Runs => match commands::lookup(&invocation.name) {
            Some(cmd) => {
                let action = action_for(cmd.name);
                let arg = invocation.arg.trim();
                if takes_argument(action) && !arg.is_empty() {
                    Outcome::RunWith(action, arg.to_string())
                } else {
                    Outcome::Run(action)
                }
            }
            // `support` said Runs, so `lookup` returning nothing would mean the
            // two disagree. Saying "unknown" is the safe read; the parity test
            // in this module fails first if it ever happens.
            None => Outcome::Unknown {
                typed: invocation.name,
                suggestion: None,
            },
        },
        Support::Missing => Outcome::Missing(
            commands::lookup(&invocation.name)
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
fn action_for(name: &str) -> GuiAction {
    match name {
        "/theme" => GuiAction::OpenThemes,
        "/evolve" => GuiAction::Evolve,
        "/usage" => GuiAction::OpenUsage,
        "/help" => GuiAction::ToggleHelp,
        "/new" => GuiAction::NewSession,
        "/stop" => GuiAction::StopTurn,
        "/sessions" => GuiAction::OpenSessions,
        "/models" => GuiAction::OpenModels,
        "/mission-control" => GuiAction::OpenMissionControl,
        "/onboard" => GuiAction::OpenOnboard("/onboard"),
        "/channels" => GuiAction::OpenOnboard("/channels"),
        "/voice" => GuiAction::OpenOnboard("/voice"),
        "/image" => GuiAction::OpenOnboard("/image"),
        "/daemon" => GuiAction::OpenOnboard("/daemon"),
        "/brain" => GuiAction::OpenOnboard("/brain"),
        "/doctor" => GuiAction::OpenOnboard("/doctor"),
        "/exit" => GuiAction::Quit,
        // Unreachable through `parse_command`: only names that `support`
        // reports as Runs arrive here. Opening the ledger is the closest thing
        // to harmless if a future edit breaks that.
        _ => GuiAction::OpenUsage,
    }
}

/// Whether an action reads the text after the command. The
/// `/models provider/model` switch is the first one that does.
fn takes_argument(action: GuiAction) -> bool {
    // `/channels whatsapp` opens that channel's dialog instead of the menu, the
    // same argument the terminal reads off the full line (#271).
    matches!(
        action,
        GuiAction::OpenModels | GuiAction::OpenOnboard("/channels")
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
    fn a_real_command_without_a_screen_says_so() {
        // Both are real: the window must not pretend they do not exist, and
        // must not hand them to the model either. `/onboard` and the step
        // commands are no longer in this set; they run in the window now.
        assert_eq!(
            parse_command("/workspace"),
            Some(Outcome::Missing("/workspace"))
        );
        assert_eq!(
            parse_command("/compact"),
            Some(Outcome::Missing("/compact"))
        );
        let line = message(&parse_command("/compact").expect("a command")).expect("a line");
        assert!(line.contains("/compact"), "{line}");
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
}
