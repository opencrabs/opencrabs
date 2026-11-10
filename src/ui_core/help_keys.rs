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

use super::palette::{PaletteAction, PaletteItem, PaletteKind, Surface};

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

/// Which frontend's supported chords Seeker may offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpClient {
    Tui,
    Gui,
}

impl HelpClient {
    fn chord(self, row: &HelpRow) -> Option<&'static str> {
        match self {
            Self::Tui => row.tui,
            Self::Gui => row.gui,
        }
    }
}

/// One documented chord and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpRow {
    /// The block this row is printed under.
    pub section: HelpSection,
    /// Machine spelling, lowercase, `+` between modifiers and the key.
    pub neutral: &'static str,
    /// How the terminal spells it, or `None` when the terminal has no such
    /// binding yet. `None` mirrors `gui`: a surface prints the chords it
    /// answers and nothing else (#2438).
    pub tui: Option<&'static str>,
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
        tui: Some(tui),
        gui,
        description,
    }
}

/// A chord the window answers and the terminal does not (#2438). The row
/// prints in the window's help and is skipped by the terminal's, which is
/// the honest shape: a terminal that lists a chord it ignores is worse than
/// one that stays quiet.
const fn window_row(
    section: HelpSection,
    neutral: &'static str,
    gui: &'static str,
    description: &'static str,
) -> HelpRow {
    HelpRow {
        section,
        neutral,
        tui: None,
        gui: Some(gui),
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
/// usage ledger in the terminal, `ctrl+u`, and in the window `ctrl-shift-u`, so
/// `ctrl-u` can be the composer's delete-to-line-start in both (#2430). The
/// usage row prints `/usage` in the terminal and `ctrl-shift-u` here. The sidebar answers
/// `ctrl+s` on both; `ctrl+l` stays a hidden alias in the terminal.
pub const ROWS: &[HelpRow] = &[
    // ── GLOBAL ──
    // The card opens from anywhere, so it belongs at the top of the help
    // rather than under the session list, where the last block ran off the
    // bottom of the screen (#2463). The chord is alt+space: alt arrives as
    // the ESC prefix, so every platform sends it cleanly (#2491).
    row(
        HelpSection::Global,
        "alt+space",
        "Alt+Space",
        Some("alt-space"),
        "Seeker: find/docs/topic/pin/ask",
    ),
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
        "Themes: choose the palette",
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
        Some("ctrl-k"),
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
        Some("ctrl-shift-u"),
        "tokens, cost and context",
    ),
    // The window's rem zoom (#2425). The terminal has no rem to scale: font
    // size there belongs to the emulator, so these rows are window-only.
    window_row(
        HelpSection::Global,
        "ctrl+plus",
        "ctrl-plus",
        "Zoom the window in",
    ),
    window_row(
        HelpSection::Global,
        "ctrl+minus",
        "ctrl-minus",
        "Zoom the window out",
    ),
    window_row(
        HelpSection::Global,
        "ctrl+zero",
        "ctrl-zero",
        "Zoom back to 100%",
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
        Some("ctrl-j"),
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
    // The split panes (#2438, #2471, #2474). The terminal splits from the
    // sessions screen with a bare glyph and the window splits from anywhere
    // with the chord, so each row carries both spellings and both help
    // screens print it from this one table instead of keeping a copy.
    row(
        HelpSection::Chat,
        "ctrl+shift+|",
        "| (sessions)",
        Some("ctrl-shift-|"),
        "Split the chat side by side",
    ),
    row(
        HelpSection::Chat,
        "ctrl+shift+_",
        "_ (sessions)",
        Some("ctrl-shift-_"),
        "Split the chat stacked",
    ),
    row(
        HelpSection::Chat,
        "alt+tab",
        "Alt+Tab",
        Some("alt-tab"),
        "Rotate panes clockwise",
    ),
    row(
        HelpSection::Chat,
        "alt+shift+tab",
        "Alt+Shift+Tab",
        Some("alt-shift-tab"),
        "Rotate panes anticlockwise",
    ),
    row(
        HelpSection::Chat,
        "ctrl+x",
        "Ctrl+X",
        Some("ctrl-x"),
        "Close the pane holding the caret",
    ),
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
        Some("ctrl-w"),
        "Delete word (vim)",
    ),
    row(
        HelpSection::InputEditing,
        "ctrl+u",
        "Ctrl+U",
        Some("ctrl-u"),
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
        "ctrl+shift+p",
        "Ctrl+Shift+P",
        Some("ctrl-shift-p"),
        "Pin the selected session",
    ),
    row(
        HelpSection::Sessions,
        "ctrl+d",
        "D",
        Some("ctrl-d"),
        "Delete session (asks first)",
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
        "r",
        "R",
        Some("r"),
        "Rename the selected session",
    ),
    row(HelpSection::Sessions, "p", "P", None, "Browse projects"),
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
        Some("t"),
        "Topics on, then a new topic",
    ),
    row(
        HelpSection::Sessions,
        "g",
        "G",
        None,
        "Group sessions under this one",
    ),
    row(
        HelpSection::Sessions,
        "c",
        "C",
        Some("c"),
        "Close or reopen the selected topic",
    ),
    row(
        HelpSection::Sessions,
        "z",
        "Z",
        None,
        "Fold or unfold the topic tree",
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

/// Build Seeker rows from the chords this client actually supports. Every
/// row opens the existing help surface; the chord and description supply the
/// searchable knowledge without adding another command.
pub fn seeker_items(client: HelpClient) -> Vec<PaletteItem> {
    ROWS.iter()
        .filter_map(|row| {
            let chord = client.chord(row)?;
            let mut keywords = format!(
                "help key keyboard shortcut {} {} {}",
                row.neutral,
                chord,
                row.section.title()
            );
            match row.neutral {
                "alt+t" => keywords.push_str(
                    " theme themes appearance appearances color colour change open switch customize customization preview",
                ),
                "ctrl+s" => keywords.push_str(
                    " sidebar side bar open show reveal close hide collapse sessions session list rail drawer",
                ),
                _ => {}
            }
            Some(
                PaletteItem::new(
                    format!("/help {}", row.description),
                    format!("{} ({chord})", row.description),
                    PaletteKind::Help,
                    PaletteAction::OpenSurface(Surface::Help),
                )
                .with_keywords(&keywords),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_core::palette::PaletteState;

    /// The TUI labels, written out so a row cannot vanish during a
    /// refactor without a test going red. Kept as data rather than derived
    /// from [`ROWS`], which would make the assertion true by construction.
    /// A slice, not a sized array: the row set grows, the length must follow
    /// it instead of being a second hand-maintained count.
    const TUI_LABELS: &[&str] = &[
        "Alt+Space",
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
        "| (sessions)",
        "_ (sessions)",
        "Alt+Tab",
        "Alt+Shift+Tab",
        "Ctrl+X",
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
        "Ctrl+Shift+P",
        "D",
        "F",
        "R",
        "P",
        "Esc",
        "T",
        "G",
        "C",
        "Z",
    ];

    #[test]
    fn every_documented_tui_chord_is_present() {
        let actual: Vec<&str> = rows().filter_map(|r| r.tui).collect();
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
                    // `|` and `_` are the split chords' key glyphs: shift
                    // and the glyph is the whole chord (#2438).
                    || matches!(c, '+' | '/' | '@' | '|' | '_')),
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
                    .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | '/' | '|' | '_')),
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
        assert_eq!(help.tui, Some("/help"));
        assert_eq!(help.gui, Some("ctrl-h"));
        assert!(
            !rows().any(|r| r.tui.is_some_and(|t| t.eq_ignore_ascii_case("ctrl+h"))),
            "ctrl+h is a word delete in a terminal, not the help door"
        );
    }

    #[test]
    fn the_card_chord_is_in_the_first_block_on_both_surfaces() {
        // #2463: the card opens on every screen, so it prints at the top of
        // the help instead of under SESSIONS, where it ran off the bottom of
        // the terminal's screen and off the window's first page.
        let row = find("alt+space").expect("the card chord must be documented");
        assert_eq!(row.section, HelpSection::Global);
        assert_eq!(row.tui, Some("Alt+Space"));
        assert_eq!(row.gui, Some("alt-space"));
        assert_eq!(row.description, "Seeker: find/docs/topic/pin/ask");
    }

    #[test]
    fn the_window_zoom_chords_are_listed_and_the_terminal_stays_quiet() {
        // The window scales its rem (#2425); a terminal's font size belongs
        // to the emulator, so the terminal's help must not claim those keys
        // (#2438) while the window's must list them.
        for (neutral, gui) in [
            ("ctrl+plus", "ctrl-plus"),
            ("ctrl+minus", "ctrl-minus"),
            ("ctrl+zero", "ctrl-zero"),
        ] {
            let row = find(neutral).unwrap_or_else(|| panic!("{neutral} is missing"));
            assert_eq!(row.gui, Some(gui), "{neutral} lost its window spelling");
            assert_eq!(row.tui, None, "{neutral} is not a terminal chord");
            assert_eq!(row.section, HelpSection::Global);
        }
    }

    #[test]
    fn seeker_rows_convert_every_supported_entry_with_unique_titles() {
        for client in [HelpClient::Tui, HelpClient::Gui] {
            let supported: Vec<&HelpRow> = ROWS
                .iter()
                .filter(|row| client.chord(row).is_some())
                .collect();
            let items = seeker_items(client);
            assert_eq!(items.len(), supported.len());
            let mut titles = std::collections::HashSet::new();

            for (row, item) in supported.into_iter().zip(items) {
                let chord = client.chord(row).expect("supported row");
                assert_eq!(item.title, format!("/help {}", row.description));
                assert_eq!(item.subtitle, format!("{} ({chord})", row.description));
                assert_eq!(item.kind, PaletteKind::Help);
                assert_eq!(item.action, PaletteAction::OpenSurface(Surface::Help));
                assert!(titles.insert(item.title), "duplicate help title");
            }
        }
    }

    #[test]
    fn seeker_rows_never_offer_another_clients_chords() {
        let tui = seeker_items(HelpClient::Tui);
        let gui = seeker_items(HelpClient::Gui);

        // The window answers `r` since #2670, so both surfaces list it now.
        assert!(tui.iter().any(|item| item.title.contains("Rename")));
        assert!(gui.iter().any(|item| item.title.contains("Rename")));
        // The window scales its own rem (#2425) and the terminal has no say.
        assert!(gui.iter().any(|item| item.title.contains("Zoom")));
        assert!(!tui.iter().any(|item| item.title.contains("Zoom")));
        // The window has no projects screen yet, so `p` stays terminal only
        // until #2669 gives it one. Flip this assertion with that landing.
        assert!(tui.iter().any(|item| item.title.contains("projects")));
        assert!(!gui.iter().any(|item| item.title.contains("projects")));

        let terminal_help = tui
            .iter()
            .find(|item| item.title == "/help This help")
            .expect("terminal help row");
        let window_help = gui
            .iter()
            .find(|item| item.title == "/help This help")
            .expect("window help row");
        assert!(terminal_help.subtitle.ends_with("(/help)"));
        assert!(window_help.subtitle.ends_with("(ctrl-h)"));
    }

    #[test]
    fn seeker_frontends_select_their_own_help_rows() {
        let gui = include_str!("../gui/main.rs");
        let tui = include_str!("../tui/app/state.rs");
        assert!(gui.contains("help_keys::seeker_items(help_keys::HelpClient::Gui)"));
        assert!(tui.contains("help_keys::seeker_items(HelpClient::Tui)"));
    }

    #[test]
    fn seeker_help_answers_theme_and_sidebar_action_language() {
        for client in [HelpClient::Tui, HelpClient::Gui] {
            for query in ["change theme", "open theme", "switch appearance"] {
                let mut state = PaletteState::new(seeker_items(client));
                state.insert_text(query);
                assert!(
                    state
                        .visible()
                        .iter()
                        .any(|row| { row.item.title == "/help Themes: choose the palette" }),
                    "{client:?} missed {query}"
                );
            }
            for query in ["open sidebar", "close sidebar", "open/close sidebar"] {
                let mut state = PaletteState::new(seeker_items(client));
                state.insert_text(query);
                assert!(
                    state
                        .visible()
                        .iter()
                        .any(|row| row.item.title == "/help List sessions"),
                    "{client:?} missed {query}"
                );
            }
        }
    }
}
