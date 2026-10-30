//! Per-dialog scoped keymaps — the single source of truth for which keys
//! do what inside each modal surface (#1775).
//!
//! Before this module, every dialog hand-rolled its own footer `Span` list
//! (three formats, five stylings), and nothing tied the displayed hints to
//! the keys the input handler actually consumed, so the two drifted: the
//! Inbox detail popup showed a CLI command that is not even on the PATH,
//! and the tool-approval dialog advertised none of its D/Esc deny keys.
//!
//! Now one table per dialog feeds both directions: the render side builds
//! every footer and the Ctrl+C command panel from [`dialog_keys`], and the
//! drift tests replay each declared event into the surface's real input
//! decision function to prove the handler still consumes it. A key that
//! stops being handled fails the test; a hint that was never true cannot
//! be written.

use crossterm::event::{KeyCode, KeyModifiers};

/// Which dialog's keys we are describing. One variant per surface that
/// takes over input while open — the ticket's locked definition of
/// "modal = dialog = popup". Full-area views (help, settings, usage)
/// count: they capture input and Esc returns, which is the behavior even
/// if the chrome is a mode switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogScope {
    FilePicker,
    DirectoryPicker,
    RestartPending,
    UpdatePrompt,
    ThemePicker,
    PlanOverlay,
    Help,
    Settings,
    UsageDashboard,
    /// The usage dashboard's cache price breakdown, open over the grid
    /// (#2027). Esc backs out to the dashboard, and that is the only key
    /// it has.
    UsageCacheBreakdown,
    SkillsDialog,
    ProfilesDialog,
    MissionControl,
    /// Inbox proposal detail — the one detail popup with verbs.
    McDetailPopup,
    /// Activity / schedule / analytics detail — read-and-close only.
    McDetailRead,
    McLogViewer,
    /// Inline tool approval (Yes / Always / No) in the chat.
    ToolApproval,
    /// The `/approve` policy selector menu.
    ApprovePolicyMenu,
    /// The SSH password prompt. No inline footer (it renders inside the
    /// input area); the scope exists so Ctrl+C can show its keys.
    SshPassword,
    /// The `/sessions` list. A full-screen surface, so it belongs in the
    /// census that `#1775` left it out of (#2162).
    Sessions,
}

impl DialogScope {
    /// Human name shown in the Ctrl+C command panel title.
    pub fn title(self) -> &'static str {
        match self {
            DialogScope::FilePicker => "File picker",
            DialogScope::DirectoryPicker => "Directory picker",
            DialogScope::RestartPending => "Restart",
            DialogScope::UpdatePrompt => "Update",
            DialogScope::ThemePicker => "Theme picker",
            DialogScope::PlanOverlay => "Plan",
            DialogScope::Help => "Help",
            DialogScope::Settings => "Settings",
            DialogScope::UsageDashboard => "Usage",
            DialogScope::UsageCacheBreakdown => "Cache prices",
            DialogScope::SkillsDialog => "Skills",
            DialogScope::ProfilesDialog => "Profiles",
            DialogScope::MissionControl => "Mission Control",
            DialogScope::McDetailPopup => "Inbox detail",
            DialogScope::McDetailRead => "Detail",
            DialogScope::McLogViewer => "Log viewer",
            DialogScope::ToolApproval => "Tool approval",
            DialogScope::ApprovePolicyMenu => "Approval policy",
            DialogScope::SshPassword => "SSH password",
            DialogScope::Sessions => "Sessions",
        }
    }
}

/// One key binding: the events that fire it, plus how it is displayed.
///
/// `Copy` so a surface that shows a state-filtered subset of its table can
/// hand the footer the entries directly (#2027).
#[derive(Debug, Clone, Copy)]
pub struct DialogKey {
    /// Key events the surface's input handler recognizes for this
    /// binding. The drift tests replay each of these into the handler.
    /// Empty for pseudo-bindings ("type to filter") that have no single
    /// key event. No runtime code reads this: it exists for the
    /// `#[cfg(test)]` drift tests, which is the whole point of the
    /// catalog — the footer can only advertise keys the handler consumes.
    #[allow(dead_code)] // read exclusively by the drift tests in src/tests/
    pub events: &'static [(KeyCode, KeyModifiers)],
    /// Footer display form, e.g. `a`, `Esc`, `↑↓`.
    pub label: &'static str,
    /// What the key does, e.g. `apply`.
    pub verb: &'static str,
}

const fn k(code: KeyCode) -> (KeyCode, KeyModifiers) {
    (code, KeyModifiers::NONE)
}

/// The scoped keymap for `scope`, in footer display order.
///
/// Order is part of the contract: the footer renders exactly this order,
/// and where a surface shows a state-dependent subset (plan overlay), the
/// subsets are suffixes so slicing stays honest.
const TABLE_1: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Up), k(KeyCode::Down)],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter), k(KeyCode::Tab)],
        label: "Enter/Tab",
        verb: "select",
    },
    DialogKey {
        events: &[],
        label: "type",
        verb: "filter",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "cancel",
    },
];

const TABLE_2: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Up), k(KeyCode::Down)],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "open",
    },
    DialogKey {
        events: &[k(KeyCode::Char(' ')), k(KeyCode::Tab)],
        label: "Space/Tab",
        verb: "select here",
    },
    DialogKey {
        events: &[k(KeyCode::Char('.')), k(KeyCode::Char('>'))],
        label: ".",
        verb: "hidden",
    },
    DialogKey {
        events: &[],
        label: "type",
        verb: "filter",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "cancel",
    },
];

const TABLE_3: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "confirm",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "cancel",
    },
];

const TABLE_4: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::PageUp),
            k(KeyCode::PageDown),
        ],
        label: "↑↓",
        verb: "preview",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "apply",
    },
    DialogKey {
        events: &[k(KeyCode::Esc), k(KeyCode::Char('q'))],
        label: "Esc/q",
        verb: "cancel",
    },
];

const TABLE_5: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Char('a'))],
        label: "a",
        verb: "approve",
    },
    DialogKey {
        events: &[k(KeyCode::Char('d'))],
        label: "d",
        verb: "discard",
    },
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::PageUp),
            k(KeyCode::PageDown),
        ],
        label: "↑↓",
        verb: "scroll",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_6: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::PageUp),
            k(KeyCode::PageDown),
        ],
        label: "↑↓",
        verb: "scroll",
    },
    DialogKey {
        events: &[k(KeyCode::Char('/'))],
        label: "/",
        verb: "search",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "back",
    },
];

const TABLE_7: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::PageUp),
            k(KeyCode::PageDown),
        ],
        label: "↑↓",
        verb: "scroll",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "back",
    },
];

const TABLE_8: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Tab), k(KeyCode::BackTab)],
        label: "Tab",
        verb: "switch",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "details",
    },
    DialogKey {
        events: &[
            k(KeyCode::Char('t')),
            k(KeyCode::Char('w')),
            k(KeyCode::Char('m')),
            k(KeyCode::Char('a')),
        ],
        label: "T/W/M/A",
        verb: "period",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_9: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Tab), k(KeyCode::Up), k(KeyCode::Down)],
        label: "Tab/↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "run",
    },
    DialogKey {
        events: &[],
        label: "type",
        verb: "filter",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_10: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Char('n'))],
        label: "n",
        verb: "new",
    },
    DialogKey {
        events: &[k(KeyCode::Char('d'))],
        label: "d",
        verb: "delete",
    },
    DialogKey {
        events: &[k(KeyCode::Char('m'))],
        label: "m",
        verb: "migrate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "switch",
    },
    DialogKey {
        events: &[k(KeyCode::Tab), k(KeyCode::Up), k(KeyCode::Down)],
        label: "Tab/↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[],
        label: "type",
        verb: "filter",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_11: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Tab),
            (KeyCode::BackTab, KeyModifiers::SHIFT),
            k(KeyCode::Char('h')),
            k(KeyCode::Char('l')),
        ],
        label: "Tab/h/l",
        verb: "switch panel",
    },
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::Char('k')),
            k(KeyCode::Char('j')),
        ],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "detail",
    },
    DialogKey {
        events: &[k(KeyCode::Char('a'))],
        label: "a",
        verb: "apply",
    },
    DialogKey {
        events: &[k(KeyCode::Char('r'))],
        label: "r",
        verb: "reject",
    },
    DialogKey {
        events: &[
            k(KeyCode::Char('d')),
            k(KeyCode::Char('w')),
            k(KeyCode::Char('m')),
            k(KeyCode::Char('A')),
        ],
        label: "D/W/M/A",
        verb: "filter",
    },
    DialogKey {
        events: &[k(KeyCode::Char('L'))],
        label: "L",
        verb: "logs",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_12: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::Char('k')),
            k(KeyCode::Char('j')),
        ],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Char('a'))],
        label: "a",
        verb: "apply",
    },
    DialogKey {
        events: &[k(KeyCode::Char('r'))],
        label: "r",
        verb: "reject",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_13: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::Char('k')),
            k(KeyCode::Char('j')),
        ],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_14: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Char('/'))],
        label: "/",
        verb: "search",
    },
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::PageUp),
            k(KeyCode::PageDown),
        ],
        label: "↑↓",
        verb: "scroll",
    },
    DialogKey {
        events: &[
            k(KeyCode::Home),
            k(KeyCode::Char('g')),
            k(KeyCode::End),
            k(KeyCode::Char('G')),
        ],
        label: "g/G",
        verb: "top/bottom",
    },
    DialogKey {
        events: &[k(KeyCode::Char('[')), k(KeyCode::Char(']'))],
        label: "[ ]",
        verb: "file",
    },
    DialogKey {
        events: &[
            k(KeyCode::Char('1')),
            k(KeyCode::Char('2')),
            k(KeyCode::Char('3')),
            k(KeyCode::Char('4')),
            k(KeyCode::Char('5')),
            k(KeyCode::Char('e')),
            k(KeyCode::Char('w')),
            k(KeyCode::Char('i')),
            k(KeyCode::Char('d')),
            k(KeyCode::Char('t')),
        ],
        label: "1-5/e/w/i/d",
        verb: "level",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "close",
    },
];

const TABLE_15: &[DialogKey] = &[
    DialogKey {
        events: &[
            k(KeyCode::Up),
            k(KeyCode::Down),
            k(KeyCode::Left),
            k(KeyCode::Right),
        ],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Char('a'))],
        label: "a",
        verb: "approve",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "confirm",
    },
    DialogKey {
        events: &[
            k(KeyCode::Char('D')),
            k(KeyCode::Char('r')),
            k(KeyCode::Esc),
        ],
        label: "D/r/Esc",
        verb: "deny",
    },
    DialogKey {
        events: &[k(KeyCode::Char('V'))],
        label: "V",
        verb: "details",
    },
];

const TABLE_16: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Up), k(KeyCode::Down)],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "confirm",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "cancel",
    },
];

const TABLE_17: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "submit",
    },
    DialogKey {
        events: &[],
        label: "type",
        verb: "password",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "cancel",
    },
];

/// The cache price breakdown on the usage dashboard (#2027). The table is a
/// detail view over the grid: the dashboard's own keys are inert while it is
/// open, so Esc is the whole keymap.
const TABLE_18: &[DialogKey] = &[DialogKey {
    events: &[k(KeyCode::Esc)],
    label: "Esc",
    verb: "close",
}];

/// The `/sessions` list (#2162). Display order is the footer's order AND
/// its priority order: the footer drops bindings from the tail when the
/// terminal is narrow, so the keys that matter most (navigate, open, get
/// out) come first. Every event here is replayed into
/// `handle_sessions_key` by the drift test, so a binding that stops being
/// handled fails the build rather than sitting in the footer as a lie.
const TABLE_19: &[DialogKey] = &[
    DialogKey {
        events: &[k(KeyCode::Up), k(KeyCode::Down)],
        label: "↑↓",
        verb: "navigate",
    },
    DialogKey {
        events: &[k(KeyCode::Enter)],
        label: "Enter",
        verb: "select",
    },
    DialogKey {
        events: &[k(KeyCode::Esc)],
        label: "Esc",
        verb: "back",
    },
    DialogKey {
        events: &[k(KeyCode::Char('n'))],
        label: "n",
        verb: "new",
    },
    DialogKey {
        events: &[k(KeyCode::Char('r'))],
        label: "r",
        verb: "rename",
    },
    DialogKey {
        events: &[k(KeyCode::Char('d'))],
        label: "d",
        verb: "delete",
    },
    DialogKey {
        events: &[k(KeyCode::Char('f'))],
        label: "f",
        verb: "files",
    },
    DialogKey {
        events: &[k(KeyCode::Char('p'))],
        label: "p",
        verb: "projects",
    },
    DialogKey {
        events: &[k(KeyCode::Char('|'))],
        label: "|",
        verb: "split h",
    },
    DialogKey {
        events: &[k(KeyCode::Char('_'))],
        label: "_",
        verb: "split v",
    },
    DialogKey {
        events: &[k(KeyCode::Char('/'))],
        label: "/",
        verb: "search",
    },
];

pub fn dialog_keys(scope: DialogScope) -> &'static [DialogKey] {
    match scope {
        DialogScope::FilePicker => TABLE_1,
        DialogScope::DirectoryPicker => TABLE_2,
        DialogScope::RestartPending | DialogScope::UpdatePrompt => TABLE_3,
        DialogScope::ThemePicker => TABLE_4,
        // Suffix-ordered: every plan state shows a true suffix of this
        // table (PostInitEditing: all; Active / PreInitEditing: from
        // "d"; NoPlan: from "Esc").
        DialogScope::PlanOverlay => TABLE_5,
        DialogScope::Help => TABLE_6,
        DialogScope::Settings => TABLE_7,
        DialogScope::UsageDashboard => TABLE_8,
        DialogScope::UsageCacheBreakdown => TABLE_18,
        DialogScope::SkillsDialog => TABLE_9,
        DialogScope::ProfilesDialog => TABLE_10,
        DialogScope::MissionControl => TABLE_11,
        DialogScope::McDetailPopup => TABLE_12,
        DialogScope::McDetailRead => TABLE_13,
        DialogScope::McLogViewer => TABLE_14,
        DialogScope::ToolApproval => TABLE_15,
        DialogScope::ApprovePolicyMenu => TABLE_16,
        DialogScope::SshPassword => TABLE_17,
        DialogScope::Sessions => TABLE_19,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::mission_control::McPanel;
    use crate::tui::app::mission_control::input::{KeyOutcome, decide};
    use crate::tui::app::mission_control::state::McState;

    /// Every scope has a non-empty table with unique labels and non-empty
    /// verbs — a scope with an empty or duplicated table renders a broken
    /// footer or command panel.
    #[test]
    fn every_scope_has_a_unique_table() {
        for scope in all_scopes() {
            let keys = dialog_keys(scope);
            assert!(!keys.is_empty(), "{scope:?} has no keys");
            let mut labels: Vec<&str> = keys.iter().map(|k| k.label).collect();
            labels.sort_unstable();
            let n = labels.len();
            labels.dedup();
            assert_eq!(labels.len(), n, "{scope:?} has duplicate labels");
            for key in keys {
                assert!(!key.verb.is_empty(), "{scope:?} empty verb");
            }
        }
    }

    /// The list of every scope, so catalog-wide tests cannot silently skip
    /// a variant added later.
    fn all_scopes() -> Vec<DialogScope> {
        vec![
            DialogScope::FilePicker,
            DialogScope::DirectoryPicker,
            DialogScope::RestartPending,
            DialogScope::UpdatePrompt,
            DialogScope::ThemePicker,
            DialogScope::PlanOverlay,
            DialogScope::Help,
            DialogScope::Settings,
            DialogScope::UsageDashboard,
            DialogScope::UsageCacheBreakdown,
            DialogScope::SkillsDialog,
            DialogScope::ProfilesDialog,
            DialogScope::MissionControl,
            DialogScope::McDetailPopup,
            DialogScope::McDetailRead,
            DialogScope::McLogViewer,
            DialogScope::ToolApproval,
            DialogScope::ApprovePolicyMenu,
            DialogScope::SshPassword,
            DialogScope::Sessions,
        ]
    }

    /// DRIFT TEST — the footer's contract with the input handler.
    ///
    /// For every Mission Control key the footer advertises, replaying it
    /// through the real `decide()` must not fall through as unhandled.
    /// Each event replays against a fresh clone of the base state: the
    /// table is display order, not a user keystroke sequence, and one
    /// advertised key (Enter) opens the popup that would swallow the
    /// next one's routing.
    #[test]
    fn mission_control_footer_keys_are_all_consumed() {
        let base = McState::default();
        let count = 3;
        for key in dialog_keys(DialogScope::MissionControl) {
            for (code, mods) in key.events {
                let mut state = base.clone();
                let event = crossterm::event::KeyEvent::new(*code, *mods);
                let outcome = decide(&mut state, count, event);
                assert_ne!(
                    outcome,
                    KeyOutcome::NotConsumed,
                    "footer advertises {:?} ({}) but decide() does not consume it",
                    code,
                    key.label
                );
            }
        }
    }

    /// DRIFT TEST for the detail popup scope, including the a/r verbs the
    /// popup footer advertises — the exact gap the ticket was filed on:
    /// the popup showed apply/reject guidance while its handler ignored
    /// both keys.
    #[test]
    fn detail_popup_footer_keys_are_all_consumed() {
        let make = |focused| McState {
            detail_open: true,
            focused_panel: focused,
            ..McState::default()
        };
        let count = 2;
        for scope in [DialogScope::McDetailPopup, DialogScope::McDetailRead] {
            for key in dialog_keys(scope) {
                for (code, mods) in key.events {
                    // Fresh state per event: Esc closes the popup and
                    // would re-route every later key.
                    let mut state = make(McPanel::Inbox);
                    let event = crossterm::event::KeyEvent::new(*code, *mods);
                    let outcome = decide(&mut state, count, event);
                    assert_ne!(
                        outcome,
                        KeyOutcome::NotConsumed,
                        "{scope:?}: footer advertises {:?} ({}) but decide_with_popup does not consume it",
                        code,
                        key.label
                    );
                }
            }
        }
    }

    /// The apply/reject verbs must actually fire from inside the popup,
    /// not merely be swallowed.
    #[test]
    fn detail_popup_apply_and_reject_fire() {
        use crossterm::event::KeyCode;
        let mut state = McState {
            detail_open: true,
            ..Default::default()
        };
        let count = 2;
        let apply = decide(
            &mut state,
            count,
            crossterm::event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(apply, KeyOutcome::ApplySelected);
        let reject = decide(
            &mut state,
            count,
            crossterm::event::KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert_eq!(reject, KeyOutcome::RejectSelected);
    }

    /// DRIFT TEST for the log viewer scope. Fresh state per event: `/`
    /// arms search mode and every later printable key would become
    /// search text rather than its own binding.
    #[test]
    fn log_viewer_footer_keys_are_all_consumed() {
        let make = || McState {
            log_viewer: Some(crate::tui::app::mission_control::log_viewer::LogViewerState::open()),
            ..McState::default()
        };
        for key in dialog_keys(DialogScope::McLogViewer) {
            for (code, mods) in key.events {
                let mut state = make();
                let event = crossterm::event::KeyEvent::new(*code, *mods);
                let outcome = decide(&mut state, 5, event);
                assert_ne!(
                    outcome,
                    KeyOutcome::NotConsumed,
                    "log viewer advertises {:?} ({}) but decide_in_log_viewer does not consume it",
                    code,
                    key.label
                );
            }
        }
    }

    /// Plan overlay subsets are suffixes of the table, so a state-filtered
    /// footer can never invent a key the full table does not know.
    #[test]
    fn plan_overlay_state_slices_are_suffixes() {
        let keys = dialog_keys(DialogScope::PlanOverlay);
        // PostInitEditing: the whole table.
        // Active / PreInitEditing: everything from "d".
        let from_d = keys.iter().position(|k| k.label == "d").unwrap();
        // NoPlan: only Esc.
        let from_esc = keys.iter().position(|k| k.label == "Esc").unwrap();
        assert!(from_d < from_esc);
    }

    /// DRIFT TEST for the cache price breakdown scope (#2027): the single key
    /// it advertises must actually close the breakdown.
    #[test]
    fn cache_breakdown_footer_key_closes_the_breakdown() {
        use crate::usage::dashboard::{CACHE_CARD, DashboardState, UsageKeyOutcome, decide};

        for key in dialog_keys(DialogScope::UsageCacheBreakdown) {
            for (code, mods) in key.events {
                let mut state = DashboardState {
                    focused_card: CACHE_CARD,
                    cache_detail_open: true,
                    ..DashboardState::default()
                };
                let event = crossterm::event::KeyEvent::new(*code, *mods);
                assert_eq!(
                    decide(&mut state, event),
                    UsageKeyOutcome::Consumed,
                    "cache breakdown advertises {code:?} ({}) but decide() does not consume it",
                    key.label
                );
                assert!(
                    !state.cache_detail_open,
                    "{code:?} is advertised as close but leaves the breakdown open"
                );
            }
        }
    }
}
