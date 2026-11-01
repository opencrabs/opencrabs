//! The help key table, spelled once for every frontend.
//!
//! The TUI and the GUI answer the same commands, but no two surfaces spell a
//! chord the same way: the TUI writes `Ctrl+N`, a desktop window writes
//! `ctrl-n`, and a browser writes `Ctrl+N` again. Keeping one list per
//! surface means a new chord is documented in one place and missing in the
//! other until somebody notices, which is exactly how the two surfaces
//! drifted apart (#2319).
//!
//! So the rows live here, as data, with three fields that matter:
//!
//! - [`HelpRow::neutral`] is the machine spelling (`ctrl+n`), stable enough
//!   for a test or a lookup to reason about.
//! - [`HelpRow::tui`] is what the terminal prints (`Ctrl+N`).
//! - [`HelpRow::gui`] is what the window prints (`ctrl-n`), or `None` when
//!   the window does not answer that chord yet. `None` is deliberate: a
//!   help screen that lists a chord the surface ignores is worse than one
//!   that leaves it out, so each ported chord flips its own row to `Some`.
//!
//! Nothing here depends on a terminal crate or a window crate, so both frontends
//! link this file as-is.
//!
//! The ABOUT block (version, provider, model) is not here on purpose: those
//! are live values read from the running app, not keys, and they carry no
//! chord to render.

/// Which block of the help screen a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelpSection {
    /// Keys that work everywhere, including outside the editor.
    Global,
    /// Keys that act on the conversation.
    Chat,
    /// Keys that edit the input line.
    InputEditing,
    /// Keys that act on the session list: the terminal's sessions screen
    /// and the window's rail (#2355).
    Sessions,
}

impl HelpSection {
    /// The heading each surface prints above this block.
    pub fn title(self) -> &'static str {
        match self {
            Self::Global => "GLOBAL",
            Self::Chat => "CHAT",
            Self::InputEditing => "INPUT EDITING",
            Self::Sessions => "SESSIONS",
        }
    }

    /// Every section, in the order the help screen shows them.
    pub const ALL: [Self; 4] = [Self::Global, Self::Chat, Self::InputEditing, Self::Sessions];
}

/// One documented chord and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpRow {
    /// The block this row is printed under.
    pub section: HelpSection,
    /// Machine spelling, lowercase, `+` between modifiers and the key.
    pub neutral: &'static str,
    /// How the terminal spells it.
    pub tui: &'static str,
    /// How the window spells it, or `None` while the window has no binding.
    pub gui: Option<&'static str>,
    /// What the chord does.
    pub description: &'static str,
}

const fn row(
    section: HelpSection,
    neutral: &'static str,
    tui: &'static str,
    gui: Option<&'static str>,
    description: &'static str,
) -> HelpRow {
    HelpRow {
        section,
        neutral,
        tui,
        gui,
        description,
    }
}

/// The rows both frontends render, in display order.
///
/// The TUI's `Ctrl+H` is missing from the GLOBAL block on purpose: in a
/// terminal that key is a word delete (it is how many terminals encode
/// `Ctrl+Backspace`, `state.rs:3805`), so the terminal opens help with
/// `/help` while the window answers `ctrl+h`.
///
/// Two rows deliberately carry different chords per surface. `Ctrl+U` is the
/// editor's delete-to-line-start in the terminal (`state.rs:3868`) and the
/// usage ledger in the window, which has no line-start delete yet, so the
/// usage row prints `/usage` there and `ctrl-u` here. The sidebar answers
/// `ctrl+s` on both; `ctrl+l` stays a hidden alias in the terminal.
pub const ROWS: &[HelpRow] = &[
    // ── GLOBAL ──
    row(
        HelpSection::Global,
        "ctrl+c",
        "Ctrl+C",
        None,
        "Snap bottom / clear, quit (2x)",
    ),
    row(
        HelpSection::Global,
        "ctrl+n",
        "Ctrl+N",
        Some("ctrl-n"),
        "New session",
    ),
    row(
        HelpSection::Global,
        "alt+t",
        "Alt+T",
        Some("ctrl-shift-t"),
        "Themes: preview and keep a theme",
    ),
    row(
        HelpSection::Global,
        "ctrl+s",
        "Ctrl+S",
        Some("ctrl-s"),
        "List sessions",
    ),
    row(
        HelpSection::Global,
        "ctrl+o",
        "Ctrl+O",
        Some("ctrl-o"),
        "Fold/unfold newest turn",
    ),
    row(
        HelpSection::Global,
        "ctrl+k",
        "Ctrl+K",
        None,
        "Clear session",
    ),
    row(
        HelpSection::Global,
        "click",
        "Mouse click",
        None,
        "Open URL/path under click",
    ),
    row(
        HelpSection::Global,
        "help",
        "/help",
        Some("ctrl-h"),
        "This help",
    ),
    row(
        HelpSection::Global,
        "usage",
        "/usage",
        Some("ctrl-u"),
        "Usage ledger",
    ),
    // ── CHAT ──
    row(
        HelpSection::Chat,
        "enter",
        "Enter",
        Some("enter"),
        "Send message",
    ),
    row(
        HelpSection::Chat,
        "shift+enter",
        "Shift+Enter",
        Some("shift-enter"),
        "New line",
    ),
    row(
        HelpSection::Chat,
        "ctrl+j",
        "Ctrl+J",
        None,
        "New line (alt)",
    ),
    row(
        HelpSection::Chat,
        "esc",
        "Escape (x2)",
        Some("esc"),
        "Cancel / abort immediately",
    ),
    row(
        HelpSection::Chat,
        "pageup/pagedown",
        "Page Up/Down",
        None,
        "Scroll history",
    ),
    row(HelpSection::Chat, "at", "@", None, "File picker"),
    // ── INPUT EDITING ──
    row(
        HelpSection::InputEditing,
        "up/down",
        "↑ / ↓",
        Some("up/down"),
        "Line nav / start-end / history",
    ),
    row(
        HelpSection::InputEditing,
        "left/right",
        "← / →",
        Some("left/right"),
        "Move cursor",
    ),
    row(
        HelpSection::InputEditing,
        "ctrl+alt+left/right",
        "Ctrl/Alt+←→",
        Some("alt-left/right"),
        "Jump word",
    ),
    row(
        HelpSection::InputEditing,
        "home/end",
        "Home / End",
        Some("home/end"),
        "Start / end of line",
    ),
    row(
        HelpSection::InputEditing,
        "alt+backspace",
        "Alt+Backspace",
        Some("alt-backspace"),
        "Delete word",
    ),
    row(
        HelpSection::InputEditing,
        "ctrl+backspace",
        "Ctrl+Backspace",
        None,
        "Delete word (alias)",
    ),
    row(
        HelpSection::InputEditing,
        "ctrl+w",
        "Ctrl+W",
        None,
        "Delete word (vim)",
    ),
    row(
        HelpSection::InputEditing,
        "ctrl+u",
        "Ctrl+U",
        None,
        "Delete to line start (vim)",
    ),
    // ── SESSIONS ──
    // Every chord here is answered on both surfaces, each with its own
    // spelling. The window's rail answers arrows only while it owns the
    // keyboard (idle esc hands it over); the terminal answers them on its
    // sessions screen.
    row(
        HelpSection::Sessions,
        "up/down",
        "↑ / ↓",
        Some("up/down"),
        "Move the highlight in the list",
    ),
    row(
        HelpSection::Sessions,
        "enter",
        "Enter",
        Some("enter"),
        "Open the highlighted session",
    ),
    row(
        HelpSection::Sessions,
        "ctrl+f",
        "/",
        Some("ctrl-f"),
        "Find a session",
    ),
    row(
        HelpSection::Sessions,
        "ctrl+shift+f",
        "Ctrl+F",
        Some("ctrl-shift-f"),
        "Pin the selected session",
    ),
    row(
        HelpSection::Sessions,
        "ctrl+d",
        "D",
        Some("ctrl-d"),
        "Delete the selected session",
    ),
    row(
        HelpSection::Sessions,
        "ctrl+g",
        "F",
        Some("ctrl-g"),
        "Session files",
    ),
    row(
        HelpSection::Sessions,
        "esc",
        "Esc",
        Some("esc"),
        "Cancel or step back",
    ),
    row(
        HelpSection::Sessions,
        "t",
        "T",
        None,
        "New topic under this session",
    ),
    row(
        HelpSection::Sessions,
        "c",
        "C",
        None,
        "Close or reopen the selected topic",
    ),
];

/// Every row in display order.
pub fn rows() -> impl Iterator<Item = &'static HelpRow> {
    ROWS.iter()
}

/// The rows of one block, in display order.
pub fn section_rows(section: HelpSection) -> impl Iterator<Item = &'static HelpRow> {
    ROWS.iter().filter(move |r| r.section == section)
}

/// The rows the window can actually answer, which is what its help shows.
pub fn gui_rows() -> impl Iterator<Item = &'static HelpRow> {
    ROWS.iter().filter(|r| r.gui.is_some())
}

/// Look a row up by its machine spelling.
pub fn find(neutral: &str) -> Option<&'static HelpRow> {
    ROWS.iter().find(|r| r.neutral == neutral)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TUI labels, written out so a row cannot vanish during a
    /// refactor without a test going red. Kept as data rather than derived
    /// from [`ROWS`], which would make the assertion true by construction.
    /// A slice, not a sized array: the row set grows, the length must follow
    /// it instead of being a second hand-maintained count.
    const TUI_LABELS: &[&str] = &[
        "Ctrl+C",
        "Ctrl+N",
        "Alt+T",
        "Ctrl+S",
        "Ctrl+O",
        "Ctrl+K",
        "Mouse click",
        "/help",
        "/usage",
        "Enter",
        "Shift+Enter",
        "Ctrl+J",
        "Escape (x2)",
        "Page Up/Down",
        "@",
        "↑ / ↓",
        "← / →",
        "Ctrl/Alt+←→",
        "Home / End",
        "Alt+Backspace",
        "Ctrl+Backspace",
        "Ctrl+W",
        "Ctrl+U",
        "↑ / ↓",
        "Enter",
        "/",
        "Ctrl+F",
        "D",
        "F",
        "Esc",
        "T",
        "C",
    ];

    #[test]
    fn every_documented_tui_chord_is_present() {
        let actual: Vec<&str> = rows().map(|r| r.tui).collect();
        assert_eq!(actual, TUI_LABELS.to_vec());
    }

    #[test]
    fn each_section_has_rows_and_they_are_in_order() {
        for section in HelpSection::ALL {
            assert!(
                section_rows(section).next().is_some(),
                "{} has no rows",
                section.title()
            );
        }
        let first_chat = rows().position(|r| r.section == HelpSection::Chat);
        let first_input = rows().position(|r| r.section == HelpSection::InputEditing);
        assert!(
            matches!((first_chat, first_input), (Some(c), Some(i)) if c < i),
            "sections must run Global, Chat, Input editing"
        );
    }

    #[test]
    fn neutral_spellings_are_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for r in rows() {
            assert!(
                r.neutral.chars().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '+' | '/' | '@')),
                "{} is not a machine spelling",
                r.neutral
            );
            // Uniqueness is per section: the same physical key answers two
            // commands depending on where the keyboard is (enter sends in
            // the chat and opens in the list), and both meanings deserve
            // their row. `find` resolves the first, so globally unique
            // chords stay the lookup keys.
            assert!(
                seen.insert((r.section, r.neutral)),
                "{} appears twice under {}",
                r.neutral,
                r.section.title()
            );
        }
    }

    #[test]
    fn gui_spellings_are_lowercase_with_hyphens() {
        for r in gui_rows() {
            let chord = r.gui.expect("gui_rows only yields rows with a chord");
            assert!(
                chord
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | '/')),
                "{chord} is not a window spelling"
            );
        }
    }

    #[test]
    fn the_window_reads_its_three_command_chords_like_the_terminal() {
        // #2319: the window used to answer cmd-n / cmd-b / cmd-o while the
        // terminal answered ctrl-n / ctrl-l / ctrl-o. Same action, different
        // chord, and switching surfaces broke muscle memory. ctrl+s is the
        // spelling both surfaces settled on for the session list, with
        // ctrl+l kept as the terminal's alias.
        assert_eq!(find("ctrl+n").and_then(|r| r.gui), Some("ctrl-n"));
        assert_eq!(find("ctrl+s").and_then(|r| r.gui), Some("ctrl-s"));
        assert_eq!(find("ctrl+o").and_then(|r| r.gui), Some("ctrl-o"));
    }

    #[test]
    fn the_terminal_opens_help_with_a_slash_command_not_ctrl_h() {
        // Ctrl+H is a word delete in a terminal (state.rs:3805), so the terminal
        // must not advertise it as the help door.
        let help = find("help").expect("the help row must exist");
        assert_eq!(help.tui, "/help");
        assert_eq!(help.gui, Some("ctrl-h"));
        assert!(
            !rows().any(|r| r.tui.eq_ignore_ascii_case("ctrl+h")),
            "ctrl+h is a word delete in a terminal, not the help door"
        );
    }
}
