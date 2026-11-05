//! The commands whose answer is a line, not a screen.
//!
//! The window used to refuse every one of these with "terminal only", which
//! was false: the engine behind them is surface-agnostic and the channels
//! answer them all (#2469). What each one needs is different, so this module
//! says WHICH kind of answer it is and owns the wording, while the callers
//! own the effects:
//!
//! * [`classify`] maps a name from the shared table to a command;
//! * the `*_reply` functions build the text for the ones that answer in words,
//!   so the wording is testable without a running engine;
//! * the bridge and the window dispatch the ones that need a turn or a screen.
//!
//! Nothing here parses a line: `ui_core::commands` already split it, and a
//! second parser is exactly how the frontends drift apart.

/// A command the window answers with text, or with a turn it composes itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextCommand {
    /// Send the compaction instruction as a turn.
    Compact,
    /// Cut the agent's context, keeping the history on screen.
    Clear,
    /// Token savings statistics.
    Rtk,
    /// Enter plan mode, optionally with the planning query.
    Plan,
    /// Report the live plan.
    ShowPlan,
    /// Approve the design plan and seed the checklist.
    Execute,
    /// Discard the live plan.
    Discard,
    /// Show or set the tool approval policy.
    Approve,
    /// Report or change the conversation's working directory.
    Cd,
    /// Set or track an autonomous goal.
    Goal,
    /// List the profiles.
    Profiles,
    /// Cycle the process.
    Restart,
    /// Install or update WhisperCrabs and launch its floating mic.
    Whisper,
    /// Create a Telegram cowork workspace through the agent's own tool.
    Cowork,
}

/// Every name this module answers.
///
/// Test-only, and deliberately so: the production path is [`classify`], which
/// reads the same names off the shared table. This list exists for the parity
/// test in `gui::commands` to walk, so a command wired here but left out of
/// `GUI_SUPPORTED` fails the suite instead of answering "terminal only" at
/// runtime (#2469). A `pub const` the binary never reads is dead code, and
/// `-D warnings` is right to say so.
#[cfg(test)]
pub const NAMES: &[&str] = &[
    "/compact",
    "/clear",
    "/rtk",
    "/plan",
    "/show-plan",
    "/execute",
    "/discard",
    "/approve",
    "/cd",
    "/goal",
    "/profiles",
    "/restart",
    "/whisper",
    "/cowork",
];

/// The command behind a name, or `None` when this module does not answer it.
pub fn classify(name: &str) -> Option<TextCommand> {
    Some(match name {
        "/compact" => TextCommand::Compact,
        "/clear" => TextCommand::Clear,
        "/rtk" => TextCommand::Rtk,
        "/plan" => TextCommand::Plan,
        "/show-plan" => TextCommand::ShowPlan,
        "/execute" => TextCommand::Execute,
        "/discard" => TextCommand::Discard,
        "/approve" => TextCommand::Approve,
        "/cd" => TextCommand::Cd,
        "/goal" => TextCommand::Goal,
        "/profiles" => TextCommand::Profiles,
        "/restart" => TextCommand::Restart,
        "/whisper" => TextCommand::Whisper,
        "/cowork" => TextCommand::Cowork,
        _ => return None,
    })
}

/// The canonical name for a command, so the bridge receives the line the
/// table spells rather than whatever alias the user typed.
pub fn name(command: TextCommand) -> &'static str {
    match command {
        TextCommand::Compact => "/compact",
        TextCommand::Clear => "/clear",
        TextCommand::Rtk => "/rtk",
        TextCommand::Plan => "/plan",
        TextCommand::ShowPlan => "/show-plan",
        TextCommand::Execute => "/execute",
        TextCommand::Discard => "/discard",
        TextCommand::Approve => "/approve",
        TextCommand::Cd => "/cd",
        TextCommand::Goal => "/goal",
        TextCommand::Profiles => "/profiles",
        TextCommand::Restart => "/restart",
        TextCommand::Whisper => "/whisper",
        TextCommand::Cowork => "/cowork",
    }
}

/// What `/whisper` says once the floating mic is up.
///
/// The words are the terminal's: the same install, the same window that
/// follows it, so a user who has seen one frontend recognises the other.
pub fn whisper_reply() -> String {
    "WhisperCrabs is running! A floating mic button is now on your screen.\n\n\
     Speak from any app. The transcription is auto-copied to your clipboard, \
     so just paste wherever you need it."
        .to_string()
}

/// Why `/whisper` could not start, with the reason kept.
pub fn whisper_failed(reason: &str) -> String {
    format!("WhisperCrabs setup failed: {reason}")
}

/// The turn `/cowork` asks for.
///
/// Cowork is a Telegram group feature with no screen of its own in either
/// frontend: the agent calls `cowork_connect`, which builds the deep link and
/// registers the session. A named workspace is created straight away; a bare
/// `/cowork` asks for the name first, exactly as the terminal does.
pub fn cowork_prompt(arg: &str) -> String {
    let name = arg.trim();
    if name.is_empty() {
        "[SYSTEM: The user wants to create a Telegram cowork workspace. Ask them \
         for a workspace name, then call the cowork_connect tool with it and show \
         them the deep link it returns.]"
            .to_string()
    } else {
        format!(
            "[SYSTEM: Create a Telegram cowork workspace named '{name}': call the \
             cowork_connect tool with workspace_name='{name}', then show the user \
             the deep link it returns so they can tap it to create the group.]"
        )
    }
}

/// The policy a bare `/approve` reports.
pub fn approval_reply(policy: &str) -> String {
    let meaning = match policy {
        "auto-always" => "every tool runs without asking",
        "auto-session" => "tools run without asking for this session only",
        _ => "every tool that changes something asks first",
    };
    format!(
        "Approval policy: {policy} ({meaning}).\n\
         Change it with /approve ask, /approve auto-session or /approve auto-always."
    )
}

/// The stored value an `/approve` argument names.
pub fn approval_target(arg: &str) -> Option<&'static str> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "ask" | "always" => Some("ask"),
        "session" | "auto-session" => Some("auto-session"),
        "yolo" | "auto" | "auto-always" => Some("auto-always"),
        _ => None,
    }
}

/// The line `/approve <policy>` prints after the write.
pub fn approval_set(policy: &str) -> String {
    format!("Approval policy set to {policy}.")
}

/// A line an `/approve` argument that names no policy gets.
pub fn approval_unknown(arg: &str) -> String {
    format!("{arg} is not an approval policy. Use ask, auto-session or auto-always.")
}

/// The profiles listing.
///
/// `entries` is `(name, description)`; `current` is the profile this process
/// is running under.
pub fn profiles_reply(entries: &[(String, Option<String>)], current: &str) -> String {
    if entries.is_empty() {
        return format!("No profiles are registered. This process runs as {current}.");
    }
    let mut out = format!("Profiles (running as {current}):\n");
    for (name, description) in entries {
        let mark = if name == current { " *" } else { "" };
        match description {
            Some(text) if !text.is_empty() => {
                out.push_str(&format!("  {name}{mark}  {text}\n"));
            }
            _ => out.push_str(&format!("  {name}{mark}\n")),
        }
    }
    out.push_str("Switching profiles needs a relaunch (opencrabs -p <name>).");
    out
}

/// What `/cd` says after a directory change.
pub fn cd_reply(path: &str) -> String {
    format!("Working directory: {path}")
}

/// What `/cd` says when the argument is not a directory.
pub fn cd_not_a_directory(arg: &str) -> String {
    format!("Not a directory: {arg}")
}

/// The RTK statistics block.
pub fn rtk_reply(succeeded: bool, stdout: &str, stderr: &str) -> String {
    let body = if succeeded {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let body = if body.is_empty() { "(no output)" } else { body };
    if succeeded {
        format!("RTK token savings:\n{body}")
    } else {
        format!("RTK gain failed:\n{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the module answers classifies, and a name outside the set
    /// does not. This is the test the dispatcher leans on: a command added to
    /// [`NAMES`] without an arm fails here first.
    #[test]
    fn text_command_names_all_classify() {
        for name in NAMES {
            assert!(
                classify(name).is_some(),
                "{name} is listed but classifies to nothing"
            );
        }
        assert_eq!(classify("/theme"), None);
        assert_eq!(classify("/not-a-command"), None);
    }

    /// A command and its canonical name round-trip, so the bridge is never
    /// handed a name the table does not spell.
    #[test]
    fn text_command_names_round_trip() {
        for line in NAMES {
            let command = classify(line).expect("listed");
            assert_eq!(name(command), *line, "{line}");
        }
    }

    /// The policies the terminal writes are the ones this window reads, so a
    /// window user sees the same three names the menu shows.
    #[test]
    fn text_command_approval_policies_round_trip() {
        for policy in ["ask", "auto-session", "auto-always"] {
            assert_eq!(approval_target(policy), Some(policy), "{policy}");
        }
        assert_eq!(approval_target("yolo"), Some("auto-always"));
        assert_eq!(approval_target("session"), Some("auto-session"));
        assert_eq!(approval_target("sometimes"), None);
        assert!(approval_reply("auto-session").contains("auto-session"));
        assert!(approval_unknown("sometimes").contains("auto-session"));
    }

    /// The profiles listing marks the running profile and says the truth about
    /// switching: this process cannot swap its own profile without a relaunch.
    #[test]
    fn text_command_profiles_listing_marks_the_running_profile() {
        let entries = vec![
            (
                "default".to_string(),
                Some("the shipped profile".to_string()),
            ),
            ("work".to_string(), None),
        ];
        let out = profiles_reply(&entries, "work");
        assert!(out.contains("work *"), "{out}");
        assert!(out.contains("default"), "{out}");
        assert!(out.contains("relaunch"), "{out}");
        // An empty registry still names the profile in use instead of a blank.
        assert!(profiles_reply(&[], "default").contains("default"));
    }

    /// A failed RTK read says so and keeps the stderr; a successful one never
    /// swallows an empty body into a blank line.
    #[test]
    fn text_command_rtk_reports_failure_and_empty_output() {
        assert!(rtk_reply(false, "", "rtk: not found").contains("rtk: not found"));
        assert!(rtk_reply(true, "", "").contains("(no output)"));
        assert!(rtk_reply(true, "saved 42%", "").contains("saved 42%"));
    }

    /// `/cowork` never invents a workspace name and never sends the bare word
    /// to the model: a named run carries the name into the tool call, a bare
    /// one asks for it first.
    #[test]
    fn text_command_cowork_composes_the_tool_turn() {
        let named = cowork_prompt("  sprint-42 ");
        assert!(named.contains("cowork_connect"), "{named}");
        assert!(named.contains("workspace_name='sprint-42'"), "{named}");
        let bare = cowork_prompt("   ");
        assert!(bare.contains("Ask them"), "{bare}");
        assert!(!bare.contains("workspace_name="), "{bare}");
    }

    /// A failed `/whisper` keeps the reason; a good one says what appeared on
    /// screen, so the user knows where to look.
    #[test]
    fn text_command_whisper_says_where_it_landed() {
        assert!(whisper_reply().contains("floating mic"));
        assert!(whisper_failed("no asset").contains("no asset"));
    }

    /// `/cd` reports the directory it landed in, or refuses the one it did not,
    /// in the same words the terminal uses. A bare `/cd` has no text reply on
    /// the window any more (#2527): the picker screen owns it.
    #[test]
    fn text_command_cd_reports_the_directory() {
        assert_eq!(cd_reply("/opt/x"), "Working directory: /opt/x");
        assert_eq!(cd_not_a_directory("nope"), "Not a directory: nope");
    }
}
