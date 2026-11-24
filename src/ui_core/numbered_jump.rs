//! The numbered jump (#3166): `cmd/ctrl + 1..9` opens the nth entry of what
//! the user is looking at, and `0` reaches the tenth entry.
//!
//! The owner's ruling, spelled once so the terminal and the desktop window
//! cannot drift:
//!
//! - `1`..`9` map to positions 1..9 and `0` maps to position 10. Digits are
//!   positions, never ids: the lists are live, so a reorder reshuffles what
//!   each chord opens ("if they flip order that is it").
//! - When the session list is what is on screen (the terminal's sessions
//!   screen, or the window's rail with keyboard focus) the chord always
//!   targets the nth visible session row.
//! - Otherwise, when the open session has live topics, the chord targets the
//!   topic bar order: position 1 is `All` (the root conversation itself),
//!   position 2 is the first topic, and the open session never changes
//!   ("sessions with topics won't change the session but topics").
//! - Otherwise the chord targets the nth visible session row, because a
//!   session without topics has no bar to jump into.
//!
//! Everything here is pure id arithmetic over lists the caller supplies in
//! live on-screen order. No terminal crate, no window crate, so both
//! frontends link this file as-is.

/// Which list a numbered chord acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpScope {
    /// The topic bar order of the open session: `All` first, then topics.
    Topics,
    /// The visible session rows, in live list order.
    Sessions,
}

/// The position a jump digit stands for.
///
/// `1`..`9` are positions 1..9. `0` is position 10, one past the single
/// digits, so ten entries stay reachable with the number row.
pub fn position_for_digit(digit: char) -> Option<usize> {
    match digit {
        '1'..='9' => Some(digit as usize - '0' as usize),
        '0' => Some(10),
        _ => None,
    }
}

/// The position a chord targets, or `None` when it is not a jump chord.
///
/// The chord needs cmd or ctrl plus a jump digit. Plain digits type into the
/// composer and must never jump; letters, punctuation and the modifier-free
/// number row are equally not jumps.
pub fn chord_position(control: bool, command: bool, digit: char) -> Option<usize> {
    if !(control || command) {
        return None;
    }
    position_for_digit(digit)
}

/// Which list the chord acts on.
///
/// The session list wins whenever it is open (`ctrl+s` in the terminal, the
/// rail focused in the window): the user is looking at sessions, so the chord
/// opens sessions. Otherwise topics win when the open session has any, and
/// sessions are the fallback when it has none.
pub fn scope_for(list_open: bool, has_topics: bool) -> JumpScope {
    if list_open || !has_topics {
        JumpScope::Sessions
    } else {
        JumpScope::Topics
    }
}

/// The id the chord opens, from lists supplied in live on-screen order.
///
/// `topics` is the topic bar order: index 0 is the root conversation (`All`),
/// index 1.. are the topics in list order, so position 2 opens the first
/// topic. `sessions` is the visible session row order, so position 1 opens
/// the first row. Positions beyond a list's length open nothing: a chord that
/// invents a target is worse than one that does nothing.
pub fn resolve_target<T: Copy>(
    scope: JumpScope,
    position: usize,
    topics: &[T],
    sessions: &[T],
) -> Option<T> {
    let list = match scope {
        JumpScope::Topics => topics,
        JumpScope::Sessions => sessions,
    };
    list.get(position.checked_sub(1)?).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Ten ids, `s0` first, so every digit maps to a distinct row.
    fn sessions() -> [Uuid; 10] {
        core::array::from_fn(|i| Uuid::from_u128(i as u128))
    }

    /// Root plus four topics, the bar order: `All` first.
    fn topics(root: Uuid) -> Vec<Uuid> {
        [root]
            .into_iter()
            .chain((0..4).map(|i| Uuid::from_u128(100 + i)))
            .collect()
    }

    #[test]
    fn digits_one_to_nine_map_to_positions_one_to_nine() {
        for (digit, want) in [('1', 1), ('5', 5), ('9', 9)] {
            assert_eq!(position_for_digit(digit), Some(want), "digit {digit}");
        }
    }

    #[test]
    fn digit_zero_is_position_ten() {
        assert_eq!(position_for_digit('0'), Some(10));
    }

    #[test]
    fn non_digits_have_no_position() {
        for ch in ['a', '-', ' ', '.', 'Q'] {
            assert_eq!(position_for_digit(ch), None, "char {ch}");
        }
    }

    #[test]
    fn plain_digits_never_jump_without_a_modifier() {
        assert_eq!(chord_position(false, false, '7'), None);
        assert_eq!(chord_position(true, false, '7'), Some(7));
        assert_eq!(chord_position(false, true, '0'), Some(10));
    }

    #[test]
    fn topic_scope_wins_when_the_open_session_has_topics() {
        assert_eq!(scope_for(false, true), JumpScope::Topics);
    }

    #[test]
    fn session_scope_applies_without_topics() {
        assert_eq!(scope_for(false, false), JumpScope::Sessions);
    }

    #[test]
    fn session_list_open_always_targets_sessions() {
        assert_eq!(scope_for(true, true), JumpScope::Sessions);
        assert_eq!(scope_for(true, false), JumpScope::Sessions);
    }

    #[test]
    fn topic_position_one_is_all_the_root_conversation() {
        let root = Uuid::from_u128(7);
        let topics = topics(root);
        assert_eq!(
            resolve_target(JumpScope::Topics, 1, &topics, &sessions()),
            Some(root)
        );
    }

    #[test]
    fn topic_position_two_is_the_first_topic_in_live_order() {
        let root = Uuid::from_u128(7);
        let topics = topics(root);
        assert_eq!(
            resolve_target(JumpScope::Topics, 2, &topics, &sessions()),
            Some(Uuid::from_u128(100))
        );
    }

    #[test]
    fn session_position_one_is_the_first_visible_row() {
        assert_eq!(
            resolve_target(JumpScope::Sessions, 1, &[], &sessions()),
            Some(Uuid::from_u128(0))
        );
    }

    #[test]
    fn session_position_ten_is_digit_zero() {
        assert_eq!(
            resolve_target(JumpScope::Sessions, 10, &[], &sessions()),
            Some(Uuid::from_u128(9))
        );
    }

    #[test]
    fn selection_follows_the_supplied_live_order_exactly() {
        let root = Uuid::from_u128(7);
        let flipped: Vec<Uuid> = topics(root).into_iter().rev().collect();
        // The same ids handed over reversed open a different topic: order is
        // the caller's live order, never a sorted copy. Reversed, the bar is
        // `[103, 102, 101, 100, 7]`, so position 3 is `101`, not the `102`
        // that position 2 would open in the original order.
        assert_eq!(
            resolve_target(JumpScope::Topics, 3, &flipped, &[]),
            Some(Uuid::from_u128(101))
        );
    }

    #[test]
    fn positions_past_the_list_open_nothing() {
        let root = Uuid::from_u128(7);
        let topics = topics(root);
        // Five bar entries, so position 6 is already past the end.
        assert_eq!(resolve_target(JumpScope::Topics, 6, &topics, &[]), None);
        assert_eq!(
            resolve_target(JumpScope::Sessions, 11, &[], &sessions()),
            None
        );
        assert_eq!(
            resolve_target(JumpScope::Sessions, 0, &[], &sessions()),
            None
        );
    }
}
