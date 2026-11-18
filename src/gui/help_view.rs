//! The Help Center (#2407, #2893): three command cards over four key cards,
//! not one long column.
//!
//! The key rows come from `status::help_rows()`, which reads the shared table
//! in `ui_core::help_keys`. This module only groups them by `HelpSection` and
//! paints the groups, so the window and the terminal never spell a chord
//! twice. Nothing here invents a shortcut, a count or a section order.
//!
//! The command rows are the terminal's own three sources (#3048): the shared
//! slash table filtered to what this window runs, the skills loaded from
//! `skills/`, and the user's entries in `commands.toml`. The window used to
//! list none of them, which made its reference the weaker of the two screens.
//!
//! The card chrome follows the Theme screen: the same panel fill, the same
//! hairline border and the same concentric radius, so the reference reads as
//! part of the window instead of a config dump.

use gpui::{Div, IntoElement, ScrollHandle, SharedString, div, prelude::*, px, rems, rgb};
use opencrabs::config::gui_theme::Palette;
use opencrabs::ui_core::card::concentric_radius;
use opencrabs::ui_core::commands::gui_help_commands;
use opencrabs::ui_core::help_keys::{HelpRow, HelpSection};

use crate::palette;
use crate::status::help_rows;
use crate::theme::{Size, text};

/// One line of the command side of the reference: the name to type and what it
/// does.
///
/// Owned rather than borrowed, because two of the three lists come off the
/// disk: skills are parsed out of `skills/*/SKILL.md` and user commands out of
/// `commands.toml`, so neither can be `&'static` like the slash table.
pub(crate) struct CommandRow {
    pub(crate) name: String,
    pub(crate) description: String,
}

/// The command lists the reference prints, read once when the screen opens.
///
/// The terminal's `/help` reads these same three sources (`help_catalog`), and
/// it reads them once on entry for a reason: `load_all_skills` walks and parses
/// every skill directory, and `CommandLoader::load` parses a file. Doing that
/// per frame is the bug that module was written to fix (#1530).
pub(crate) struct CommandLists {
    pub(crate) commands: Vec<CommandRow>,
    pub(crate) skills: Vec<CommandRow>,
    pub(crate) custom: Vec<CommandRow>,
}

impl CommandLists {
    /// The closed state: nothing to print until the reference opens and reads
    /// the three sources. Cheaper than an `Option`, and it makes the renderer
    /// free of a branch that could paint a half-filled screen.
    pub(crate) fn empty() -> Self {
        Self {
            commands: Vec::new(),
            skills: Vec::new(),
            custom: Vec::new(),
        }
    }
}

/// Read the three command sources once, for one visit to the reference.
///
/// Called when the screen opens, never from the renderer. The slash rows come
/// from the shared table already filtered to what this window runs; the other
/// two are the terminal's own sources, so the two surfaces list the same names
/// in the same order (#3048).
pub(crate) fn load_commands() -> CommandLists {
    let commands = gui_help_commands()
        .into_iter()
        .map(|command| CommandRow {
            name: command.name.to_string(),
            description: command.description.to_string(),
        })
        .collect();
    let skills = opencrabs::brain::skills::load_all_skills()
        .into_iter()
        .map(|skill| CommandRow {
            name: skill.slash_name,
            description: skill.description,
        })
        .collect();
    let brain_path = opencrabs::brain::BrainLoader::resolve_path();
    let mut custom: Vec<CommandRow> = opencrabs::brain::CommandLoader::from_brain_path(&brain_path)
        .load()
        .into_iter()
        .map(|command| CommandRow {
            name: command.name,
            description: command.description,
        })
        .collect();
    // Sorted by name, the order the terminal prints them in: the file's own
    // order is an accident of when each command was added.
    custom.sort_by(|a, b| a.name.cmp(&b.name));
    CommandLists {
        commands,
        skills,
        custom,
    }
}

/// Does a command row answer the `/` filter?
///
/// The name with or without its slash, and the description, because the screen
/// exists for the reader who does not know the name: someone typing "record"
/// wants `/voice` when its description says so. An empty filter keeps
/// everything, the same rule the key groups use.
fn command_matches(row: &CommandRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.trim_start_matches('/').to_lowercase();
    row.name.to_lowercase().contains(&query) || row.description.to_lowercase().contains(&query)
}

/// What the Help Center shows: the running version, a newer release if the
/// check found one, the `/` filter, the scroll position the arrows move, and
/// whether the window is narrow enough that two columns would clip.
pub(crate) struct HelpView<'a> {
    pub(crate) version: &'static str,
    pub(crate) update: Option<String>,
    pub(crate) filter: Option<String>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) narrow: bool,
    /// The commands, skills and user commands, read once when the screen
    /// opened. Empty until the opener fills them.
    pub(crate) commands: &'a CommandLists,
}

/// One card's worth of rows: the section it prints and what survived the
/// filter. A group with no rows paints nothing.
pub(crate) struct HelpGroup {
    pub(crate) section: HelpSection,
    pub(crate) rows: Vec<&'static HelpRow>,
}

impl HelpGroup {
    fn count(&self) -> usize {
        self.rows.len()
    }
}

/// Every section in `HelpSection::ALL` order, with its rows, filtered.
///
/// The filter matches the description or the chord the window prints, which
/// is what the old single-column screen matched. Sections keep their declared
/// order even when a filter empties one out, so a narrowed screen never
/// reshuffles the cards.
pub(crate) fn groups(filter: Option<&str>) -> Vec<HelpGroup> {
    let query = filter.unwrap_or("").to_lowercase();
    let mut out: Vec<HelpGroup> = HelpSection::ALL
        .into_iter()
        .map(|section| HelpGroup {
            section,
            rows: Vec::new(),
        })
        .collect();
    let rows: Vec<_> = help_rows()
        .into_iter()
        .filter(|(_, row)| {
            query.is_empty()
                || row.description.to_lowercase().contains(&query)
                || row.gui.unwrap_or_default().to_lowercase().contains(&query)
        })
        .collect();
    for (section, row) in rows {
        if let Some(group) = out.iter_mut().find(|group| group.section == section) {
            group.rows.push(row);
        }
    }
    out
}

/// How many rows the cards will actually paint.
pub(crate) fn total(groups: &[HelpGroup]) -> usize {
    groups.iter().map(HelpGroup::count).sum()
}

/// A chord as a keycap: a compact bordered box, right-aligned in a fixed
/// column so the labels line up card to card.
fn keycap(chord: &str, shown: Palette) -> Div {
    div()
        .w(rems(8.5))
        .flex_shrink_0()
        .flex()
        .flex_row()
        .justify_end()
        .child(
            div()
                .px_1()
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(shown.track))
                .bg(rgb(shown.panel_hi))
                .text_xs()
                .text_color(rgb(shown.text_normal))
                .child(SharedString::from(chord.to_string())),
        )
}

/// One section card: a small accent spine, the section name, a live count,
/// then the keycap rows.
fn section_card(group: &HelpGroup, shown: Palette) -> Div {
    let mut rows = div().flex().flex_col().gap_2();
    for row in &group.rows {
        let chord = row.gui.unwrap_or_default();
        rows = rows.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(keycap(chord, shown))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child(SharedString::from(row.description)),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .w_full()
        .rounded(px(concentric_radius(14.0)))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(rgb(shown.panel))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .h(px(12.0))
                        .w(px(2.0))
                        .rounded(px(1.0))
                        .bg(rgb(shown.accent)),
                )
                .child(text(
                    SharedString::from(group.section.title()),
                    Size::Xs,
                    shown.text_strong,
                ))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .rounded(px(6.0))
                        .bg(rgb(shown.panel_hi))
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(group.count().to_string())),
                ),
        )
        .child(rows)
}

/// One card of commands: a name column, a description column, and the same
/// chrome the key cards wear so the reference reads as one screen.
///
/// The name is plain text in the keycap's own column width rather than a
/// keycap, because a command is a word to type, not a chord to press. The
/// column is the same 8.5rem so a name and a chord line up card to card.
///
/// A wide window splits the list in two, the same way the key grid does, so
/// the thirty-six commands of the slash table do not push the keys off the
/// screen. A narrow one keeps a single column instead of clipping a
/// description to nothing.
fn command_card(title: &'static str, rows: &[&CommandRow], shown: Palette, narrow: bool) -> Div {
    let name_column = |row: &CommandRow| {
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap_3()
            .child(
                div()
                    .w(rems(8.5))
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(shown.text_normal))
                            .child(SharedString::from(row.name.clone())),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(shown.text_muted))
                    .child(SharedString::from(row.description.clone())),
            )
    };
    let mut body = div().flex().flex_col().gap_2();
    if narrow || rows.len() < 2 {
        for row in rows {
            body = body.child(name_column(row));
        }
    } else {
        let half = rows.len().div_ceil(2);
        let (left, right) = rows.split_at(half);
        let mut columns = div().flex().flex_row().items_start().gap_4();
        for column in [left, right] {
            let mut col = div().flex().flex_col().gap_2().flex_1().min_w_0();
            for row in column {
                col = col.child(name_column(row));
            }
            columns = columns.child(col);
        }
        body = body.child(columns);
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .w_full()
        .rounded(px(concentric_radius(14.0)))
        .border_1()
        .border_color(rgb(shown.panel_hi))
        .bg(rgb(shown.panel))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .h(px(12.0))
                        .w(px(2.0))
                        .rounded(px(1.0))
                        .bg(rgb(shown.accent)),
                )
                .child(text(SharedString::from(title), Size::Xs, shown.text_strong))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .rounded(px(6.0))
                        .bg(rgb(shown.panel_hi))
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(rows.len().to_string())),
                ),
        )
        .child(body)
}

/// The header's count line: how many commands and how many shortcuts are on
/// screen, each in its singular form at one.
///
/// Both numbers come from what survived the filter, so a narrowed screen never
/// claims the full table is there.
pub(crate) fn header_counts(commands: usize, shortcuts: usize) -> String {
    let commands = if commands == 1 {
        "1 command".to_string()
    } else {
        format!("{commands} commands")
    };
    let shortcuts = if shortcuts == 1 {
        "1 shortcut".to_string()
    } else {
        format!("{shortcuts} shortcuts")
    };
    format!("{commands} · {shortcuts}")
}

/// The frame: a header, the card grid, and one footer that names the keys
/// that drive the screen. Esc is printed here once and nowhere else.
pub(crate) fn help_screen(view: &HelpView<'_>) -> impl IntoElement {
    let shown = palette::current();
    let groups = groups(view.filter.as_deref());
    let visible: Vec<&HelpGroup> = groups
        .iter()
        .filter(|group| !group.rows.is_empty())
        .collect();
    let query = view.filter.as_deref().unwrap_or("").trim();
    // The three command lists, filtered the same way the key cards are. An
    // empty result prints no card at all, the rule the key groups follow.
    let commands: Vec<&CommandRow> = view
        .commands
        .commands
        .iter()
        .filter(|row| command_matches(row, query))
        .collect();
    let skills: Vec<&CommandRow> = view
        .commands
        .skills
        .iter()
        .filter(|row| command_matches(row, query))
        .collect();
    let custom: Vec<&CommandRow> = view
        .commands
        .custom
        .iter()
        .filter(|row| command_matches(row, query))
        .collect();
    let command_total = commands.len() + skills.len() + custom.len();
    let total = total(&groups);

    let mut title = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(shown.text_faint))
                .child(SharedString::from("REFERENCE")),
        )
        .child(text(
            SharedString::from("Help Center"),
            Size::Xl,
            shown.text,
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_muted))
                        .child(SharedString::from(format!("v{}", view.version))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(header_counts(command_total, total))),
                ),
        );
    if let Some(latest) = &view.update {
        title = title.child(
            div()
                .text_xs()
                .text_color(rgb(shown.accent))
                .child(SharedString::from(format!("v{latest} available"))),
        );
    }

    let header = div()
        .flex()
        .flex_row()
        .items_start()
        .justify_between()
        .px_6()
        .pt_4()
        .pb_3()
        .child(title)
        .child(
            div()
                .text_lg()
                .text_color(rgb(shown.text_muted))
                .child(SharedString::from("🦀")),
        );

    // Two columns, filled row-major: Global and Chat share the top row,
    // Input editing and Sessions the bottom one. A narrow window collapses to
    // a single column instead of clipping a label.
    //
    // The commands cards come first, the order the terminal's `/help` prints
    // them in: people open a reference to find a command, not a chord. Each is
    // one card across the full width, because a command needs room for its
    // description, and each hides itself when the filter empties it.
    let mut body = div().flex().flex_col().gap_4().px_6().pb_4();
    for (title, rows) in [
        ("COMMANDS", &commands),
        ("SKILLS", &skills),
        ("CUSTOM COMMANDS", &custom),
    ] {
        if !rows.is_empty() {
            body = body.child(command_card(title, rows, shown, view.narrow));
        }
    }
    if view.narrow {
        for group in &visible {
            body = body.child(section_card(group, shown));
        }
    } else {
        let mut left = div().flex().flex_col().gap_4().flex_1().min_w_0();
        let mut right = div().flex().flex_col().gap_4().flex_1().min_w_0();
        for (index, group) in visible.iter().enumerate() {
            let card = section_card(group, shown);
            if index % 2 == 0 {
                left = left.child(card);
            } else {
                right = right.child(card);
            }
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_4()
                .child(left)
                .child(right),
        );
    }

    let hint = |chord: &'static str, label: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .px_1()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(shown.track))
                    .bg(rgb(shown.panel_hi))
                    .text_xs()
                    .text_color(rgb(shown.text_normal))
                    .child(SharedString::from(chord)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(shown.text_faint))
                    .child(SharedString::from(label)),
            )
    };

    let mut footer = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_6()
        .py_3()
        .border_t_1()
        .border_color(rgb(shown.panel_hi))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_4()
                .child(hint("↑↓", "scroll"))
                .child(hint("pgup/pgdn", "page"))
                .child(hint("/", "filter"))
                .child(hint("esc", "close")),
        );
    if let Some(filter) = &view.filter {
        footer = footer.child(
            div()
                .text_xs()
                .text_color(rgb(shown.accent))
                .child(SharedString::from(format!("/ {filter}"))),
        );
    }

    div()
        .id("help-screen")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(shown.bg))
        .text_color(rgb(shown.text))
        .child(header)
        .child(
            div()
                .id("help-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&view.scroll)
                .child(body),
        )
        .child(footer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::ui_core::commands::{Support, support_gui};

    /// The header counts both halves of the screen, each in its singular form
    /// at one, because a reference that says "1 shortcuts" reads like a bug
    /// (#3048).
    #[test]
    fn the_header_counts_commands_and_shortcuts() {
        assert_eq!(
            header_counts(36, 40),
            "36 commands · 40 shortcuts",
            "both numbers are named, in that order"
        );
        assert_eq!(
            header_counts(1, 1),
            "1 command · 1 shortcut",
            "one of each drops the plural s"
        );
        assert_eq!(
            header_counts(1, 12),
            "1 command · 12 shortcuts",
            "the two halves count independently"
        );
    }

    /// The `/` box finds a command by its name, with or without the slash, and
    /// by its description, so a reader who does not know the name can still
    /// reach the row (#3048).
    #[test]
    fn the_filter_finds_a_command_by_name_or_description() {
        let row = CommandRow {
            name: "/voice".to_string(),
            description: "Voice STT/TTS setup".to_string(),
        };
        assert!(command_matches(&row, ""), "an empty filter keeps every row");
        assert!(command_matches(&row, "voice"), "the name without its slash");
        assert!(command_matches(&row, "/voice"), "the name as it is typed");
        assert!(
            command_matches(&row, "VOICE"),
            "matching is case-insensitive, the way the key cards match"
        );
        assert!(
            command_matches(&row, "tts"),
            "a description word reaches the command it belongs to"
        );
        assert!(
            !command_matches(&row, "nonsense-zzz"),
            "a query that matches nothing hides the row"
        );
    }

    /// The command card lists what the window runs, and nothing it cannot:
    /// every name is one this window answers, `/seeker` is absent because no
    /// surface answers it, and the window-only `/settings` is present because
    /// the reference describes this surface, not the terminal (#3048).
    #[test]
    fn the_reference_lists_the_commands_this_window_runs() {
        let lists = load_commands();
        let names: Vec<&str> = lists.commands.iter().map(|row| row.name.as_str()).collect();
        for wanted in [
            "/voice",
            "/theme",
            "/usage",
            "/mission-control",
            "/settings",
        ] {
            assert!(
                names.contains(&wanted),
                "{wanted} is missing from the reference: {names:?}"
            );
        }
        assert!(
            !names.contains(&"/seeker"),
            "Seeker is a finder, not a command: nothing answers that name"
        );
        for row in &lists.commands {
            assert!(
                row.name.starts_with('/'),
                "a command row prints the name to type: {}",
                row.name
            );
            assert!(
                !row.description.is_empty(),
                "{} prints with no description",
                row.name
            );
            assert_eq!(
                support_gui(&row.name),
                Support::Runs,
                "{} is listed but this window does not run it",
                row.name
            );
        }
        // Skills and the user's own commands come off the disk, so they are
        // only asserted as well-formed: an empty brain has none, and that is
        // not a failure of the screen.
        for row in lists.skills.iter().chain(lists.custom.iter()) {
            assert!(
                row.name.starts_with('/'),
                "a skill or custom row prints its slash form: {}",
                row.name
            );
        }
    }
}
