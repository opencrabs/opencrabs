//! The slash commands every frontend answers, in one table.
//!
//! The terminal owned this list alone (`SLASH_COMMANDS` in `tui/app/state.rs`),
//! so a second frontend had no way to know what a command was called without
//! copying it. A copy drifts, and a drifted command list is worse than none:
//! the window offers a name the terminal never answers, or the two disagree
//! about what a name means.
//!
//! Nothing here depends on a terminal crate. The TUI re-exports these names, so
//! `SLASH_COMMANDS` keeps meaning what it did and there is exactly one list.
//!
//! `/models` and the onboarding steps are in the table because the terminal
//! answers them. Whether a given frontend can RUN one is a separate question,
//! answered by [`Support`], so a window that cannot yet do the work says so in
//! one line instead of pretending the command does not exist.

/// Slash command definition
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// Available slash commands for autocomplete
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/new",
        description: "Start a new session",
    },
    SlashCommand {
        name: "/cd",
        description: "Change working directory",
    },
    SlashCommand {
        name: "/sessions",
        description: "List all sessions",
    },
    SlashCommand {
        name: "/projects",
        description: "Browse projects and their sessions",
    },
    SlashCommand {
        name: "/compact",
        description: "Compact context now",
    },
    SlashCommand {
        name: "/clear",
        description: "Clear context here at no cost; history and title stay",
    },
    SlashCommand {
        name: "/theme",
        description: "List / set / reset the TUI color theme (/theme set dracula)",
    },
    SlashCommand {
        name: "/stop",
        description: "Abort the running turn (same as Esc x2)",
    },
    SlashCommand {
        name: "/plan",
        description: "Enter Plan mode (design a plan for approval)",
    },
    SlashCommand {
        name: "/show-plan",
        description: "Show the current plan state",
    },
    SlashCommand {
        name: "/execute",
        description: "Approve the design plan / retry the checklist seed",
    },
    SlashCommand {
        name: "/discard",
        description: "Discard the live plan",
    },
    SlashCommand {
        name: "/help",
        description: "Show available commands",
    },
    SlashCommand {
        name: "/models",
        description: "Switch model",
    },
    SlashCommand {
        name: "/usage",
        description: "Session usage stats",
    },
    SlashCommand {
        name: "/mission-control",
        description: "Mission control: analytics, activity, inbox & schedule",
    },
    SlashCommand {
        name: "/onboard",
        description: "Run setup wizard",
    },
    // Single-step setup (#1981). The legacy `/onboard:<step>` spellings
    // still resolve but are deliberately not offered here.
    SlashCommand {
        name: "/workspace",
        description: "Workspace settings",
    },
    SlashCommand {
        name: "/channels",
        description: "Setup Telegram, Slack, Discord, WhatsApp, Trello",
    },
    SlashCommand {
        name: "/voice",
        description: "Voice STT/TTS setup",
    },
    SlashCommand {
        name: "/image",
        description: "Image handling setup (vision + generation)",
    },
    SlashCommand {
        name: "/daemon",
        description: "Background service (always-on) setup",
    },
    SlashCommand {
        name: "/brain",
        description: "Brain/persona setup",
    },
    SlashCommand {
        name: "/doctor",
        description: "Run connection health check",
    },
    SlashCommand {
        name: "/cowork",
        description: "Create a Telegram cowork workspace (QR + deep link)",
    },
    SlashCommand {
        name: "/approve",
        description: "Tool approval policy",
    },
    SlashCommand {
        name: "/evolve",
        description: "Download latest release & restart",
    },
    SlashCommand {
        name: "/restart",
        description: "Restart OpenCrabs (resumes this session)",
    },
    SlashCommand {
        name: "/exit",
        description: "Exit OpenCrabs",
    },
    SlashCommand {
        name: "/whisper",
        description: "Speak anywhere, paste to clipboard",
    },
    SlashCommand {
        name: "/skills",
        description: "Browse and run loaded skills",
    },
    SlashCommand {
        name: "/rtk",
        description: "Show RTK token savings statistics",
    },
    SlashCommand {
        name: "/goal",
        description: "Set/track an autonomous goal (/goal <text>, status, pause, resume, clear)",
    },
    SlashCommand {
        name: "/profiles",
        description: "Manage OpenCrabs profiles (list, create, switch, migrate)",
    },
];

/// Commands the window answers without pretending the terminal has the same
/// screen. Keeping these out of `SLASH_COMMANDS` stops the terminal's help and
/// autocomplete advertising a modal it cannot open (#2878).
pub const GUI_ONLY_COMMANDS: &[SlashCommand] = &[SlashCommand {
    name: "/settings",
    description: "Open agent, memory and credential settings",
}];

/// Names a user may type for a command that is spelled differently in the
/// table. `/themes` is what a window user reaches for, because the screen is
/// called themes and the key chord is "themes"; the terminal has always spelled
/// the command `/theme`. Both resolve to the one table entry so neither
/// frontend grows a second name.
pub const ALIASES: &[(&str, &str)] = &[
    ("/themes", "/theme"),
    // The terminal has always answered both spellings; the window reads the
    // same table, so it answers them too (#2469).
    ("/showplan", "/show-plan"),
    ("/show_plan", "/show-plan"),
];

/// Commands this frontend runs today, checked against the table by a test so a
/// typo cannot quietly become a command that "exists" but no frontend answers.
///
/// The rest of the table is not a gap in this list; it is the parity work that
/// has its own issues (`/onboard` and its single-step screens, plan approval,
/// mission control).
pub const GUI_SUPPORTED: &[&str] = &[
    "/theme",
    "/settings",
    "/usage",
    "/help",
    "/new",
    "/stop",
    "/sessions",
    "/projects",
    "/mission-control",
    "/models",
    "/skills",
    "/onboard",
    "/workspace",
    "/channels",
    "/voice",
    "/image",
    "/daemon",
    "/brain",
    "/doctor",
    "/evolve",
    "/exit",
    // The text commands (#2469). Their answers live in `gui::text_commands`;
    // the engine behind each one is the terminal's own.
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

/// Whether a frontend can run a command it recognises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Support {
    /// This frontend runs it.
    Runs,
    /// The command exists, but this frontend has no screen for it yet.
    Missing,
    /// Not a command name at all.
    Unknown,
}

/// A submitted line split into the command word and everything after it.
///
/// `arg` is trimmed, so `/theme set dracula  ` and `/theme  set dracula` both
/// hand `set dracula` to the caller. The terminal reads its argument the same
/// way, which is why `/models provider/model` works with any spacing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    /// The first word, slash included, exactly as typed.
    pub name: String,
    /// The rest of the line, trimmed, or empty.
    pub arg: String,
}

/// Split a submitted line into a command and its argument.
///
/// `None` means "this is not a command": plain text, an empty line, or a lone
/// slash, which is what a user has typed on the way to spelling a command. The
/// caller sends `None` to the agent and never to the dispatcher.
pub fn parse(input: &str) -> Option<Invocation> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let (name, arg) = match trimmed.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (trimmed, ""),
    };
    // A bare "/" is not a command name. "." is not reachable through
    // `starts_with('/')` and is only here to make the intent explicit.
    if name.is_empty() || name == "/" {
        return None;
    }
    Some(Invocation {
        name: name.to_string(),
        arg: arg.to_string(),
    })
}

/// Every command the window can offer: the shared set plus its own screens.
pub fn gui_commands() -> impl Iterator<Item = &'static SlashCommand> {
    SLASH_COMMANDS.iter().chain(GUI_ONLY_COMMANDS)
}

/// The table entry a typed name refers to, following [`ALIASES`].
///
/// Matching is exact, the same as the terminal's dispatcher: `/Models` is not
/// `/models` there either, and a window that silently guessed would let a user
/// build a habit the terminal breaks.
pub fn lookup(name: &str) -> Option<&'static SlashCommand> {
    SLASH_COMMANDS.iter().find(|c| c.name == name).or_else(|| {
        let canonical = ALIASES
            .iter()
            .find(|(alias, _)| *alias == name)
            .map(|(_, canonical)| *canonical)?;
        SLASH_COMMANDS.iter().find(|c| c.name == canonical)
    })
}

/// The window's command lookup, including screens the terminal does not own.
pub fn lookup_gui(name: &str) -> Option<&'static SlashCommand> {
    GUI_ONLY_COMMANDS
        .iter()
        .find(|command| command.name == name)
        .or_else(|| lookup(name))
}

/// Whether this frontend runs the command, knows it and cannot run it, or has
/// never heard of it. The three answers are drawn differently: one acts, one
/// names the frontend gap, one suggests a spelling.
pub fn support(name: &str) -> Support {
    match lookup(name) {
        None => Support::Unknown,
        Some(cmd) if GUI_SUPPORTED.contains(&cmd.name) => Support::Runs,
        Some(_) => Support::Missing,
    }
}

/// The window's support answer, using its own command table as well as the
/// shared one. `/settings` runs here without becoming a terminal command.
pub fn support_gui(name: &str) -> Support {
    match lookup_gui(name) {
        None => Support::Unknown,
        Some(cmd) if GUI_SUPPORTED.contains(&cmd.name) => Support::Runs,
        Some(_) => Support::Missing,
    }
}

/// The closest command name to an unrecognised one, or `None` when nothing is
/// close enough to be worth suggesting.
///
/// Longest shared prefix wins, so a slash command typo lands on the name the
/// user was reaching for (`/them` finds `/theme`, not `/whisper`). A prefix
/// under two characters is not evidence of anything and gets no suggestion.
pub fn suggest(name: &str) -> Option<&'static str> {
    let mut best: Option<(usize, &'static str)> = None;
    for cmd in SLASH_COMMANDS {
        let shared = shared_prefix_len(name, cmd.name);
        if shared >= 2 && best.is_none_or(|(best_len, _)| shared > best_len) {
            best = Some((shared, cmd.name));
        }
    }
    best.map(|(_, name)| name)
}

/// Shared leading bytes, counted in bytes because both sides are ASCII command
/// names and a byte comparison here cannot split a character.
fn shared_prefix_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// The table names that complete a half-typed command.
///
/// Empty unless the draft is still the command word itself: one leading slash,
/// no whitespace anywhere. A space means the user has moved on to the argument,
/// and an argument is exactly where a popup must stop offering names.
///
/// Aliases are not listed. `/themes` resolves, but showing both spellings in a
/// list the user is reading is a second name for one thing, which is how a
/// short list starts to feel long.
pub fn completions(draft: &str) -> Vec<&'static str> {
    completions_from(draft, SLASH_COMMANDS.iter())
}

/// Command completion for the window, including its own settings screen.
pub fn gui_completions(draft: &str) -> Vec<&'static str> {
    completions_from(draft, gui_commands())
}

fn completions_from(
    draft: &str,
    commands: impl Iterator<Item = &'static SlashCommand>,
) -> Vec<&'static str> {
    // Leading space is indentation the user cannot see, so it is trimmed. A
    // space anywhere after the slash is the argument boundary, and a name list
    // there is noise: `/usage ` is finished, `/us` is not.
    let word = draft.trim_start();
    if !word.starts_with('/') || word.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    commands
        .map(|cmd| cmd.name)
        .filter(|name| name.starts_with(word))
        .collect()
}

/// A description cut to a column budget, never mid-character.
///
/// The completion popup is a fixed-width column. Some table descriptions are
/// long on purpose (they are the terminal's help text, and shortening them
/// would change what `/help` prints), so the popup clips instead.
pub fn clipped(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte, _)) => text.get(..byte).unwrap_or(text),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_the_name_and_the_argument() {
        let inv = parse("/models openai/gpt-5").expect("a command");
        assert_eq!(inv.name, "/models");
        assert_eq!(inv.arg, "openai/gpt-5");

        // Argument spacing is not the caller's problem: the leading run is
        // trimmed and the inside is handed over as typed, exactly like the
        // terminal's `strip_prefix(..).trim()`.
        let spaced = parse("/theme   set   dracula  ").expect("a command");
        assert_eq!(spaced.name, "/theme");
        assert_eq!(spaced.arg, "set   dracula");

        let bare = parse("  /usage  ").expect("a command");
        assert_eq!(bare.name, "/usage");
        assert_eq!(bare.arg, "");
    }

    #[test]
    fn parse_refuses_what_is_not_a_command() {
        assert!(parse("hello").is_none());
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
        // Half-typed: the user is still spelling a command.
        assert!(parse("/").is_none());
        // Anything that starts with a slash is command-shaped. Its name is
        // the whole word; whether any frontend answers it is `support`'s job,
        // and for a path the answer is "never heard of it".
        let path = parse("/usr/bin/env").expect("command-shaped");
        assert_eq!(path.name, "/usr/bin/env");
        assert_eq!(path.arg, "");
        assert_eq!(support(&path.name), Support::Unknown);
    }

    #[test]
    fn aliases_resolve_to_the_one_entry() {
        let theme = lookup("/theme").expect("/theme is in the table");
        let themes = lookup("/themes").expect("/themes is an alias");
        assert_eq!(theme.name, themes.name);
        assert_eq!(support("/themes"), Support::Runs);
    }

    #[test]
    fn support_separates_missing_from_unknown() {
        assert_eq!(support("/usage"), Support::Runs);
        // `/workspace` used to be the example of a real command with no
        // screen; the window jumps to the wizard's own workspace step now, so
        // it runs there and nothing in the table is left unanswered (#2469).
        assert_eq!(support("/workspace"), Support::Runs);
        for cmd in SLASH_COMMANDS {
            assert_ne!(
                support(cmd.name),
                Support::Missing,
                "{} is in the table but has no window screen",
                cmd.name
            );
        }
        // The window draws Mission Control now, so it runs there.
        assert_eq!(support("/mission-control"), Support::Runs);
        assert_eq!(support("/not-a-command"), Support::Unknown);
        // The terminal's rule, kept: matching is exact.
        assert_eq!(support("/Usage"), Support::Unknown);
    }

    #[test]
    fn every_supported_name_is_a_real_command() {
        for name in GUI_SUPPORTED {
            assert!(
                lookup_gui(name).is_some(),
                "{name} is listed as supported but is not in the window's table"
            );
            assert_eq!(support_gui(name), Support::Runs, "{name} does not run");
        }
    }

    #[test]
    fn settings_is_a_window_command_without_a_dead_terminal_entry() {
        assert!(lookup("/settings").is_none());
        assert_eq!(support("/settings"), Support::Unknown);
        assert_eq!(
            lookup_gui("/settings").map(|command| command.description),
            Some("Open agent, memory and credential settings")
        );
        assert_eq!(support_gui("/settings"), Support::Runs);
        assert!(gui_commands().any(|command| command.name == "/settings"));
        assert_eq!(gui_completions("/set"), vec!["/settings"]);
        assert!(completions("/set").is_empty());
    }

    #[test]
    fn suggestions_point_at_the_reached_for_name() {
        assert_eq!(suggest("/them"), Some("/theme"));
        assert_eq!(suggest("/usege"), Some("/usage"));
        assert_eq!(suggest("/zzz"), None);
        // Nothing to go on: a lone slash matches everything equally.
        assert_eq!(suggest("/"), None);
    }

    #[test]
    fn completions_only_while_spelling_the_name() {
        // A lone slash is the whole table: the user has opened the door.
        assert_eq!(completions("/").len(), SLASH_COMMANDS.len());
        assert_eq!(completions("/them"), vec!["/theme"]);
        assert_eq!(completions("/us"), vec!["/usage"]);
        // The word is finished: an argument is not a name lookup.
        assert!(completions("/usage today").is_empty());
        assert!(completions("/usage ").is_empty());
        assert!(completions("hello").is_empty());
        assert!(completions("/zzz").is_empty());
        // The alias resolves but is not offered as a second spelling.
        assert!(!completions("/").contains(&"/themes"));
    }

    #[test]
    fn clipped_never_splits_a_character() {
        assert_eq!(clipped("abc", 3), "abc");
        assert_eq!(clipped("abcdef", 3), "abc");
        // One character per char, not per byte: the accent survives or the
        // character is dropped whole, never half.
        let accented = "n\u{e9}e";
        assert_eq!(clipped(accented, 2), "n\u{e9}");
        assert_eq!(clipped(accented, 0), "");
    }

    #[test]
    fn every_description_is_present() {
        for cmd in gui_commands() {
            assert!(!cmd.name.is_empty());
            assert!(cmd.name.starts_with('/'), "{} has no slash", cmd.name);
            assert!(
                !cmd.description.is_empty(),
                "{} has no description",
                cmd.name
            );
        }
    }
}
