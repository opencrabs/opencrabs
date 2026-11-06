//! Which conversation owns a window-level live surface (#2616).
//!
//! The GUI window keeps ONE live turn and ONE busy flag, but it can open any
//! number of conversations. Without an owner, the card of a turn the user
//! walked away from paints into whatever conversation is on screen, so every
//! session appears to share the same tool block. The owner is recorded where a
//! turn starts and read against the session on screen. The card itself stays
//! where it is, so coming back to the session that owns it finds it again.

use uuid::Uuid;

/// The session a window's live turn and its busy flag belong to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LiveOwner {
    live: Option<Uuid>,
    busy: Option<Uuid>,
}

impl LiveOwner {
    /// A turn started in `session`: the card and the busy flag are its.
    pub fn begin(&mut self, session: Uuid) {
        self.live = Some(session);
        self.busy = Some(session);
    }

    /// The turn in `session` ended. Another session's ownership is untouched,
    /// so a late event cannot clear a card that was never its own.
    pub fn settle(&mut self, session: Uuid) {
        if self.live == Some(session) {
            self.live = None;
        }
        if self.busy == Some(session) {
            self.busy = None;
        }
    }

    /// The card is gone: the session was deleted or the transcript reloaded.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether the live card belongs to the conversation on screen.
    pub fn owns_live(&self, visible: Option<Uuid>) -> bool {
        self.live.is_some() && self.live == visible
    }

    /// The busy state to report for the conversation on screen. A turn left
    /// running in another session must not mark the visible one as working.
    pub fn busy_for(&self, busy: bool, visible: Option<Uuid>) -> bool {
        busy && self.busy.is_some() && self.busy == visible
    }

    /// The session holding the card, for callers that settle by id.
    pub fn live_owner(&self) -> Option<Uuid> {
        self.live
    }
}

#[cfg(test)]
mod tests {
    use super::LiveOwner;
    use uuid::Uuid;

    const A: Uuid = Uuid::from_u128(0xa);
    const B: Uuid = Uuid::from_u128(0xb);

    #[test]
    fn a_turn_owns_both_the_card_and_the_busy_flag() {
        let mut owner = LiveOwner::default();
        owner.begin(A);
        assert_eq!(owner.live_owner(), Some(A));
        assert!(owner.owns_live(Some(A)));
        assert!(owner.busy_for(true, Some(A)));
    }

    #[test]
    fn a_card_never_paints_into_another_conversation() {
        let mut owner = LiveOwner::default();
        owner.begin(A);
        assert!(!owner.owns_live(Some(B)), "B must not wear A's tool rows");
        assert!(
            !owner.busy_for(true, Some(B)),
            "A working must not mark B busy"
        );
        assert!(!owner.owns_live(None), "no conversation, no card");
    }

    #[test]
    fn settling_one_session_leaves_another_sessions_ownership_alone() {
        let mut owner = LiveOwner::default();
        owner.begin(A);
        owner.settle(B);
        assert!(owner.owns_live(Some(A)), "a stray settle must not clear A");
        owner.settle(A);
        assert!(!owner.owns_live(Some(A)));
        assert!(!owner.busy_for(true, Some(A)));
    }

    #[test]
    fn nothing_is_painted_before_a_turn_starts() {
        let owner = LiveOwner::default();
        assert!(!owner.owns_live(Some(A)));
        assert!(!owner.busy_for(true, Some(A)));
        assert_eq!(owner.live_owner(), None);
    }

    #[test]
    fn clear_drops_the_card_and_the_busy_flag() {
        let mut owner = LiveOwner::default();
        owner.begin(A);
        owner.clear();
        assert!(!owner.owns_live(Some(A)));
        assert!(!owner.busy_for(true, Some(A)));
        assert_eq!(owner, LiveOwner::default());
    }
}
