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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpSection {
    /// Keys that work everywhere, including outside the editor.
    Global,
    /// Keys that act on the conversation.
    Chat,
    /// Keys that edit the input line.
    InputEditing,
}

impl HelpSection {
    /// The heading each surface prints above this block.
    pub fn title(self) -> &'static str {
        match self {
            Self::Global => "GLOBAL",
            Self::Chat => "CHAT",
            Self::InputEditing => "INPUT EDITING",
        }
    }

    /// Every section, in the order the help screen shows them.
    pub const ALL: [Self; 3] = [Self::Global, Self::Chat, Self::InputEditing];
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
        None,
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
        None,
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
        None,
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

    /// The 22 TUI labels, written out so a row cannot vanish during a
    /// refactor without a test going red. Kept as data rather than derived
    /// from [`ROWS`], which would make the assertion true by construction.
    const TUI_LABELS: [&str; 22] = [
        "Ctrl+C",
        "Ctrl+N",
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
            assert!(seen.insert(r.neutral), "{} appears twice", r.neutral);
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
