//! Which key leaves or enters a screen, decided by one pure table.
//!
//! The window used to answer these chords inline in `on_key`, in an order that
//! mattered and was impossible to test: the branch that leaves the usage
//! ledger sat between an editor gate and a busy gate, and whether it ever ran
//! could only be learned by pressing keys in a real window (#2335 shipped on a
//! test that greps its own source). The table here takes the key, the
//! modifiers and the current screen, and returns the action. No window, no
//! focus, no context: so the routing is exercised by ordinary unit tests.

use crate::Screen;

/// What a screen chord asks the window to do. The caller owns the effect; this
/// type only names it, which is what lets the table stay pure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScreenAction {
    /// ctrl+u from anywhere that is not the ledger.
    EnterUsage,
    /// ctrl+u or esc while the ledger is showing.
    LeaveUsage,
    /// ctrl+g while the conversation is showing.
    EnterFiles,
    /// ctrl+g or esc while the files list is showing.
    LeaveFiles,
    /// esc while Mission Control is showing.
    LeaveMissionControl,
    /// ctrl+h, or Cmd+? on a Mac.
    ToggleHelp,
}

/// Where focus belongs once a screen is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusTarget {
    /// The message composer, so typing lands without a click.
    Composer,
    /// The window root, so screen keys have a known receiver rather than
    /// relying on gpui's fallback for an element that is no longer drawn.
    Root,
}

/// The modifiers that matter to the screen table, copied out of a keystroke so
/// the table does not depend on gpui's event types.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ChordMods {
    pub control: bool,
    pub shift: bool,
    /// The command key on macOS, the "platform" modifier in gpui.
    pub platform: bool,
}

/// The action a key takes on the screen showing now, or `None` when the key
/// belongs to someone else.
///
/// Escape is deliberately unclaimed on the conversation: backing out of the
/// chat into the ledger was the mixing the owner called out, and Esc must
/// never close the window.
pub fn screen_chord(key: &str, mods: ChordMods, screen: Screen) -> Option<ScreenAction> {
    if key == "escape" {
        return match screen {
            Screen::Usage => Some(ScreenAction::LeaveUsage),
            Screen::Files => Some(ScreenAction::LeaveFiles),
            Screen::MissionControl => Some(ScreenAction::LeaveMissionControl),
            Screen::Chat
            | Screen::Models
            | Screen::Skills
            | Screen::Onboard
            | Screen::DirectoryPicker => None,
        };
    }
    // ctrl+shift+u, not ctrl+u: plain ctrl+u is delete-to-line-start in the
    // composer, the TUI's binding (#2430).
    if mods.control && mods.shift && key == "u" {
        return Some(match screen {
            Screen::Usage => ScreenAction::LeaveUsage,
            _ => ScreenAction::EnterUsage,
        });
    }
    if mods.control && key == "g" {
        return Some(match screen {
            Screen::Chat => ScreenAction::EnterFiles,
            _ => ScreenAction::LeaveFiles,
        });
    }
    // Cmd+? is the same door for a hand already on the command key: the
    // platform reports it as `/` with shift, and a layout that reports the
    // glyph itself is answered too.
    let cmd_question = mods.platform && (key == "?" || (key == "/" && mods.shift));
    if (mods.control && key == "h") || cmd_question {
        return Some(ScreenAction::ToggleHelp);
    }
    None
}

/// The word the key log prints for a table answer. `none` is the signal that
/// the keystroke arrived and matched nothing, which is a different failure from
/// a keystroke that never arrived at all.
pub fn action_label(action: Option<ScreenAction>) -> &'static str {
    match action {
        Some(ScreenAction::EnterUsage) => "EnterUsage",
        Some(ScreenAction::LeaveUsage) => "LeaveUsage",
        Some(ScreenAction::EnterFiles) => "EnterFiles",
        Some(ScreenAction::LeaveFiles) => "LeaveFiles",
        Some(ScreenAction::LeaveMissionControl) => "LeaveMissionControl",
        Some(ScreenAction::ToggleHelp) => "ToggleHelp",
        None => "none",
    }
}

/// An inline overlay that can hold the keyboard while it is open. The caller
/// passes them bottom to top, so the last one is what the user is nearest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayId {
    /// The session rail, armed with Tab.
    Rail,
    /// The rail's find field, armed with ctrl+f.
    RailFind,
    /// The topic name prompt.
    TopicPrompt,
    /// The read only topic facts panel.
    TopicInfo,
    /// The key reference, filter included.
    Help,
    /// The delete confirmation.
    ConfirmDelete,
    /// A file preview, which covers the window.
    Preview,
    /// One row's slide out action strip.
    RowActions,
    /// A row's context menu.
    RailMenu,
    /// Waiting for a second session while a split is set up.
    SplitPick,
    /// Seeker.
    CommandPalette,
    /// The theme modal, which owns the window while it is open (#2619).
    Themes,
}

/// Who answers a keystroke. Exactly one of these is true at a time, which is
/// what the window lacked: every inline overlay opened while the composer
/// stayed focused, so the composer's keymap answered Enter and the arrows
/// under a field that claimed to own them (#2454, #2455).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyboardOwner {
    /// Nothing else is showing, so typing belongs to the message field.
    Composer,
    /// An inline overlay is on top and takes every key.
    Overlay(OverlayId),
    /// A full screen is showing, so its own key branch answers.
    Screen(Screen),
}

/// The one decision. Overlays win in stack order, then a screen that is not
/// the conversation, then the composer.
pub fn keyboard_owner(overlays: &[OverlayId], screen: Screen) -> KeyboardOwner {
    if let Some(top) = overlays.last() {
        return KeyboardOwner::Overlay(*top);
    }
    match screen {
        Screen::Chat => KeyboardOwner::Composer,
        other => KeyboardOwner::Screen(other),
    }
}

/// Where focus belongs for an owner: the composer only when the composer owns
/// the keyboard, the window root otherwise. Focus is the mechanism, so the
/// composer's keymap and its input handler are out of the dispatch path while
/// anything else is open, instead of each overlay patching around them.
pub fn focus_target(owner: KeyboardOwner) -> FocusTarget {
    match owner {
        KeyboardOwner::Composer => FocusTarget::Composer,
        KeyboardOwner::Overlay(_) | KeyboardOwner::Screen(_) => FocusTarget::Root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: ChordMods = ChordMods {
        control: false,
        shift: false,
        platform: false,
    };
    const CTRL_SHIFT: ChordMods = ChordMods {
        control: true,
        shift: true,
        platform: false,
    };

    const CTRL: ChordMods = ChordMods {
        control: true,
        shift: false,
        platform: false,
    };

    #[test]
    fn esc_leaves_usage() {
        assert_eq!(
            screen_chord("escape", PLAIN, Screen::Usage),
            Some(ScreenAction::LeaveUsage)
        );
    }

    #[test]
    fn ctrl_shift_u_leaves_usage() {
        assert_eq!(
            screen_chord("u", CTRL_SHIFT, Screen::Usage),
            Some(ScreenAction::LeaveUsage)
        );
    }

    #[test]
    fn esc_leaves_mission_control() {
        assert_eq!(
            screen_chord("escape", PLAIN, Screen::MissionControl),
            Some(ScreenAction::LeaveMissionControl)
        );
        assert_eq!(
            action_label(Some(ScreenAction::LeaveMissionControl)),
            "LeaveMissionControl"
        );
    }

    #[test]
    fn esc_leaves_files() {
        assert_eq!(
            screen_chord("escape", PLAIN, Screen::Files),
            Some(ScreenAction::LeaveFiles)
        );
    }

    #[test]
    fn ctrl_shift_u_enters_usage() {
        assert_eq!(
            screen_chord("u", CTRL_SHIFT, Screen::Chat),
            Some(ScreenAction::EnterUsage)
        );
        assert_eq!(
            screen_chord("u", CTRL_SHIFT, Screen::Files),
            Some(ScreenAction::EnterUsage)
        );
    }

    #[test]
    fn plain_ctrl_u_is_left_to_the_composer() {
        assert_eq!(screen_chord("u", CTRL, Screen::Chat), None);
    }

    #[test]
    fn esc_on_chat_is_unclaimed() {
        assert_eq!(screen_chord("escape", PLAIN, Screen::Chat), None);
        assert_eq!(screen_chord("escape", PLAIN, Screen::Models), None);
    }

    #[test]
    fn ctrl_g_toggles_files() {
        assert_eq!(
            screen_chord("g", CTRL, Screen::Chat),
            Some(ScreenAction::EnterFiles)
        );
        assert_eq!(
            screen_chord("g", CTRL, Screen::Files),
            Some(ScreenAction::LeaveFiles)
        );
    }

    #[test]
    fn ctrl_h_opens_the_key_reference() {
        assert_eq!(
            screen_chord("h", CTRL, Screen::Chat),
            Some(ScreenAction::ToggleHelp)
        );
    }

    #[test]
    fn cmd_question_opens_the_key_reference() {
        let cmd = ChordMods {
            control: false,
            shift: true,
            platform: true,
        };
        assert_eq!(
            screen_chord("/", cmd, Screen::Usage),
            Some(ScreenAction::ToggleHelp)
        );
    }

    #[test]
    fn plain_letters_are_unclaimed() {
        for key in ["u", "g", "h", "q", "escape"] {
            assert_eq!(screen_chord(key, PLAIN, Screen::Chat), None, "{key}");
        }
    }

    #[test]
    fn the_action_label_prints_none_for_an_unclaimed_key() {
        assert_eq!(action_label(None), "none");
        assert_eq!(action_label(Some(ScreenAction::LeaveUsage)), "LeaveUsage");
    }

    #[test]
    fn focus_for_screen_puts_the_caret_in_the_composer_on_chat() {
        assert_eq!(
            focus_target(keyboard_owner(&[], Screen::Chat)),
            FocusTarget::Composer
        );
    }

    #[test]
    fn focus_for_screen_puts_focus_on_the_root_off_chat() {
        for screen in [Screen::Usage, Screen::Files, Screen::MissionControl] {
            assert_eq!(
                focus_target(keyboard_owner(&[], screen)),
                FocusTarget::Root,
                "{screen:?}"
            );
        }
    }

    /// The owner is the topmost overlay, whatever the screen says. Before
    /// this, an overlay opened under a focused composer and the composer
    /// kept answering (#2454, #2455).
    #[test]
    fn keyboard_owner_prefers_the_topmost_overlay() {
        let overlays = [OverlayId::Rail, OverlayId::Help, OverlayId::Preview];
        assert_eq!(
            keyboard_owner(&overlays, Screen::Chat),
            KeyboardOwner::Overlay(OverlayId::Preview)
        );
        // A screen does not outrank an overlay either: Seeker opens over
        // the models screen and keeps the keys.
        assert_eq!(
            keyboard_owner(&[OverlayId::CommandPalette], Screen::Models),
            KeyboardOwner::Overlay(OverlayId::CommandPalette)
        );
    }

    #[test]
    fn keyboard_owner_falls_back_to_the_screen_then_the_composer() {
        assert_eq!(keyboard_owner(&[], Screen::Chat), KeyboardOwner::Composer);
        assert_eq!(
            keyboard_owner(&[], Screen::Models),
            KeyboardOwner::Screen(Screen::Models)
        );
        // The theme modal is an overlay, not a screen, so it outranks whichever
        // screen it was opened over (#2619).
        assert_eq!(
            keyboard_owner(&[OverlayId::Themes], Screen::MissionControl),
            KeyboardOwner::Overlay(OverlayId::Themes)
        );
    }

    /// Focus is the whole mechanism: an overlay gets the window root, so the
    /// composer's keymap is not in the dispatch path at all.
    #[test]
    fn only_the_composer_owner_focuses_the_composer() {
        for owner in [
            KeyboardOwner::Overlay(OverlayId::RailFind),
            KeyboardOwner::Overlay(OverlayId::ConfirmDelete),
            KeyboardOwner::Screen(Screen::Usage),
        ] {
            assert_eq!(focus_target(owner), FocusTarget::Root, "{owner:?}");
        }
        assert_eq!(focus_target(KeyboardOwner::Composer), FocusTarget::Composer);
    }
}
