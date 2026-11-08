//! Whether a transcript should follow its own bottom (#2685).
//!
//! The terminal keeps this as a flag (`auto_scroll` in
//! `tui/app/state.rs`) and drops it when the reader scrolls up. The window
//! has no such state: every bottom snap is one shot, so a row that lands after
//! the layout measured the content is not followed. This module holds the
//! decision as arithmetic, so the window can re-arm every frame and the rule
//! stays testable without a window.

/// How close to the bottom counts as the bottom, in px. A scroll offset lands
/// on fractional pixels, so an exact compare would read "not at the bottom"
/// on a transcript the reader never moved.
pub const FOLLOW_SLACK_PX: f32 = 2.0;

/// Whether the viewport sits at the bottom of its content, so the next row
/// that lands should stay in view.
///
/// `max_offset` is how far the content can scroll: zero when everything fits,
/// which is the bottom by definition. A non-finite value cannot be measured
/// yet, and guessing would either yank a reader to the bottom or strand them,
/// so it reads as "not following".
pub fn at_bottom(offset: f32, max_offset: f32) -> bool {
    if !offset.is_finite() || !max_offset.is_finite() {
        return false;
    }
    if max_offset <= 0.0 {
        return true;
    }
    offset >= max_offset - FOLLOW_SLACK_PX
}

/// Whether a transcript that was following its bottom should keep following
/// it after the content grew (#2685).
///
/// The reader's place is what decides this, never the fact that a turn is
/// running: a reader parked at the bottom keeps watching it, and a reader who
/// scrolled up to read history is left alone until they come back.
pub fn follows_growth(offset: f32, max_offset: f32) -> bool {
    at_bottom(offset, max_offset)
}

/// The reader's place on the rule's own scale (#2698).
///
/// GPUI's `ScrollHandle` reports the content's displacement from the viewport,
/// so the offset grows more negative as the reader walks down the transcript
/// and the maximum it can reach is positive. Every rule here compares distances
/// measured from the top, so the offset is flipped at this one door: comparing
/// the raw offset with the maximum would read "at the bottom" as never true.
pub fn scrolled_from_top(offset: f32) -> f32 {
    -offset
}

/// Whether a wheel tick is the reader walking back through history (#2698).
///
/// GPUI applies the delta to the viewport itself, so this only answers whether
/// the follow should be released on the spot. A positive vertical delta moves
/// the content down, which is the reader going up; a purely horizontal tick
/// (`0.0`) scrolls nothing vertically and releases nothing. A non-finite delta
/// is not a gesture and never costs the reader their place.
pub fn wheel_releases_follow(dy: f32) -> bool {
    dy.is_finite() && dy > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_that_fits_is_always_at_the_bottom() {
        assert!(at_bottom(0.0, 0.0));
        assert!(at_bottom(0.0, -1.0));
    }

    #[test]
    fn fractional_offsets_still_count_as_the_bottom() {
        assert!(at_bottom(998.5, 1000.0));
        assert!(at_bottom(1000.0, 1000.0));
    }

    #[test]
    fn a_reader_scrolled_up_is_not_at_the_bottom() {
        assert!(!at_bottom(200.0, 1000.0));
        assert!(!follows_growth(900.0, 1000.0));
    }

    #[test]
    fn the_handles_negative_offset_is_read_as_a_distance_from_the_top() {
        // GPUI parks the handle at `-max` when the transcript is at its
        // bottom, which is the state every one of these rules calls "here".
        assert_eq!(scrolled_from_top(-1000.0), 1000.0);
        assert_eq!(scrolled_from_top(0.0), 0.0);
        assert!(at_bottom(scrolled_from_top(-1000.0), 1000.0));
        assert!(!at_bottom(scrolled_from_top(-100.0), 1000.0));
    }

    #[test]
    fn an_unmeasured_viewport_never_yanks_the_reader() {
        assert!(!at_bottom(f32::NAN, 100.0));
        assert!(!at_bottom(0.0, f32::INFINITY));
    }
}

/// The sticky-follow state of one transcript (#2685).
///
/// A one shot snap cannot follow anything: it is consumed by the next layout,
/// and every row that lands afterwards is out of view. The state remembers
/// whether the reader wants the bottom, judged from the reader's own place and
/// never from the fact that a turn is running.
#[derive(Clone, Debug)]
pub struct Follow {
    /// The offset seen on the previous pass, so a scroll up can be told from
    /// content that grew underneath a reader who did not move.
    last_offset: f32,
    following: bool,
}

impl Default for Follow {
    fn default() -> Self {
        Self::new()
    }
}

impl Follow {
    /// A fresh transcript follows its bottom: opening a conversation lands on
    /// the newest message.
    pub fn new() -> Self {
        Self {
            last_offset: 0.0,
            following: true,
        }
    }

    /// Whether this transcript currently follows its bottom.
    pub fn is_following(&self) -> bool {
        self.following
    }

    /// Fold in the current viewport and report whether to snap to the bottom.
    ///
    /// Only a reader who moved UP releases the follow, which is what tells a
    /// deliberate scroll apart from a row landing: content growing moves
    /// `max_offset` and leaves `offset` alone, so it can never be mistaken for
    /// a reader leaving the bottom.
    pub fn update(&mut self, offset: f32, max_offset: f32) -> bool {
        let moved_up = offset.is_finite() && offset < self.last_offset - FOLLOW_SLACK_PX;
        if moved_up && !at_bottom(offset, max_offset) {
            self.following = false;
        } else if at_bottom(offset, max_offset) {
            // Back at the bottom, by the reader's own hand or because the
            // content fits again.
            self.following = true;
        }
        if offset.is_finite() {
            self.last_offset = offset;
        }
        self.following
    }

    /// Force the follow back on, for a change the reader did not ask for but
    /// needs to see: a panel that appears under the conversation and takes
    /// room from it, or a split that resizes the pane.
    pub fn rearm(&mut self) {
        self.following = true;
    }

    /// Drop the follow because the reader moved by hand: a wheel tick or a
    /// PageUp says where they want to be, and the rows that keep landing must
    /// not pull them back (#2698).
    pub fn release(&mut self) {
        self.following = false;
    }
}

#[cfg(test)]
mod follow_tests {
    use super::*;

    #[test]
    fn a_new_transcript_follows() {
        assert!(Follow::new().is_following());
    }

    #[test]
    fn growth_does_not_release_the_follow() {
        let mut f = Follow::new();
        // At the bottom, then a row lands: offset holds, the max grows.
        assert!(f.update(1000.0, 1000.0));
        assert!(f.update(1000.0, 1200.0));
    }

    #[test]
    fn a_scroll_up_releases_the_follow() {
        let mut f = Follow::new();
        f.update(1000.0, 1000.0);
        assert!(!f.update(400.0, 1200.0));
        // New rows keep landing while the reader sits in history.
        assert!(!f.update(400.0, 1600.0));
    }

    #[test]
    fn coming_back_to_the_bottom_re_arms_it() {
        let mut f = Follow::new();
        f.update(1000.0, 1000.0);
        assert!(!f.update(400.0, 1200.0));
        assert!(f.update(1200.0, 1200.0));
    }

    #[test]
    fn an_unmeasured_viewport_holds_the_state() {
        let mut f = Follow::new();
        assert!(f.update(f32::NAN, 100.0));
        assert!(f.is_following());
    }

    #[test]
    fn rearm_forces_the_bottom() {
        let mut f = Follow::new();
        f.update(1000.0, 1000.0);
        f.update(10.0, 2000.0);
        f.rearm();
        assert!(f.is_following());
    }

    #[test]
    fn a_wheel_tick_up_releases_and_a_tick_down_does_not() {
        assert!(wheel_releases_follow(1.0));
        assert!(!wheel_releases_follow(-1.0));
        assert!(!wheel_releases_follow(0.0));
        assert!(!wheel_releases_follow(f32::NAN));
    }

    #[test]
    fn release_holds_while_rows_keep_landing() {
        let mut f = Follow::new();
        f.update(1000.0, 1000.0);
        f.release();
        assert!(!f.is_following());
        // Rows land under the reader and none of them pulls them back.
        assert!(!f.update(400.0, 1200.0));
        assert!(!f.update(400.0, 1600.0));
        // Coming back down re-arms it.
        f.rearm();
        assert!(f.update(1600.0, 1600.0));
    }
}
