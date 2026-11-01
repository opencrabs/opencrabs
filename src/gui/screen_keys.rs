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
            Screen::Chat | Screen::Themes => None,
        };
    }
    if mods.control && key == "u" {
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
        Some(ScreenAction::ToggleHelp) => "ToggleHelp",
        None => "none",
    }
}

/// Focus for a screen: the composer on the conversation, the root everywhere
/// else. Both halves of the owner's report are this one decision: the caret
/// was never put back on return, and no receiver was named on entry.
pub fn focus_target(screen: Screen) -> FocusTarget {
    match screen {
        Screen::Chat => FocusTarget::Composer,
        Screen::Usage | Screen::Files | Screen::Themes => FocusTarget::Root,
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
    fn ctrl_u_leaves_usage() {
        assert_eq!(
            screen_chord("u", CTRL, Screen::Usage),
            Some(ScreenAction::LeaveUsage)
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
    fn ctrl_u_enters_usage() {
        assert_eq!(
            screen_chord("u", CTRL, Screen::Chat),
            Some(ScreenAction::EnterUsage)
        );
        assert_eq!(
            screen_chord("u", CTRL, Screen::Files),
            Some(ScreenAction::EnterUsage)
        );
    }

    #[test]
    fn esc_on_chat_is_unclaimed() {
        assert_eq!(screen_chord("escape", PLAIN, Screen::Chat), None);
        assert_eq!(screen_chord("escape", PLAIN, Screen::Themes), None);
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
        assert_eq!(focus_target(Screen::Chat), FocusTarget::Composer);
    }

    #[test]
    fn focus_for_screen_puts_focus_on_the_root_off_chat() {
        for screen in [Screen::Usage, Screen::Files, Screen::Themes] {
            assert_eq!(focus_target(screen), FocusTarget::Root, "{screen:?}");
        }
    }
}
