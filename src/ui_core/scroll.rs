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

/// Where a page key leaves one transcript, and whether that landing gives the
/// follow back (#2959).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageLanding {
    /// The offset to write, on GPUI's own scale: zero at the top, `-max_offset`
    /// at the bottom.
    pub offset: f32,
    /// Whether the landing sits at the bottom, which is the only thing that
    /// gives the follow back.
    pub at_end: bool,
}

/// Turn one page from `offset`, toward the top of the content when `up`.
///
/// The window used to clamp the offset and then ask whether the clamped value
/// had reached the bottom. The clamp guarantees it has, so the answer was
/// always yes: PageUp raised the offset and the same handler snapped it
/// straight back down, and PageDown only looked like it worked because the
/// reader was already at the end (#2959). The landing is decided here instead,
/// on the rules the wheel and the growth check already use, so the decision can
/// be tested without a window.
///
/// `max_offset` is how far the content can scroll, as a positive distance. An
/// unmeasured viewport has no page to turn: the offset is left alone and the
/// landing never claims the bottom, the same reading `at_bottom` gives a
/// non-finite measurement.
pub fn page_landing(offset: f32, max_offset: f32, page: f32, up: bool) -> PageLanding {
    if !offset.is_finite() || !max_offset.is_finite() {
        return PageLanding {
            offset,
            at_end: false,
        };
    }
    let max = max_offset.max(0.0);
    let page = if page.is_finite() { page.max(0.0) } else { 0.0 };
    let dy = if up { page } else { -page };
    let offset = (offset + dy).min(0.0).max(-max);
    PageLanding {
        offset,
        at_end: at_bottom(scrolled_from_top(offset), max),
    }
}

/// The visible thumb for one vertical scrollbar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollThumb {
    pub top: f32,
    pub height: f32,
}

/// Size and place a scrollbar thumb from the same measurements as the reader.
///
/// `offset` uses GPUI's scale: zero at the top and `-max_offset` at the bottom.
/// Content that fits has no thumb. A very long document keeps a 24px thumb so
/// the position indicator cannot shrink into an invisible speck.
pub fn vertical_scroll_thumb(
    viewport_height: f32,
    max_offset: f32,
    offset: f32,
) -> Option<ScrollThumb> {
    if !viewport_height.is_finite()
        || !max_offset.is_finite()
        || !offset.is_finite()
        || viewport_height <= 0.0
        || max_offset <= 0.0
    {
        return None;
    }
    let content_height = viewport_height + max_offset;
    let height = (viewport_height * viewport_height / content_height)
        .max(24.0)
        .min(viewport_height);
    let progress = (-offset / max_offset).clamp(0.0, 1.0);
    Some(ScrollThumb {
        top: (viewport_height - height) * progress,
        height,
    })
}

/// Pointer travel that does not scroll yet, in px (#2964).
pub const AUTO_SCROLL_DEAD_ZONE_PX: f32 = 10.0;
/// The fastest a stationary pointer can walk a transcript, in px per second.
pub const AUTO_SCROLL_MAX_SPEED_PX_PER_SEC: f32 = 1600.0;
/// A delayed frame must not turn elapsed wall time into one large jump.
const AUTO_SCROLL_MAX_FRAME_SECS: f32 = 0.1;

/// One frame of middle-click auto-scroll, on GPUI's negative-offset scale.
///
/// The pointer above the anchor raises the content toward older rows, and the
/// pointer below it lowers the content toward newer rows. Speed grows with
/// travel outside the dead zone and then caps. Non-finite input and a frame
/// with no positive elapsed time move nothing.
pub fn auto_scroll_delta(anchor_y: f32, pointer_y: f32, elapsed_secs: f32) -> f32 {
    if !anchor_y.is_finite() || !pointer_y.is_finite() || !elapsed_secs.is_finite() {
        return 0.0;
    }
    let distance = pointer_y - anchor_y;
    let travel = distance.abs() - AUTO_SCROLL_DEAD_ZONE_PX;
    if travel <= 0.0 || elapsed_secs <= 0.0 {
        return 0.0;
    }
    let speed = (travel * 6.0).min(AUTO_SCROLL_MAX_SPEED_PX_PER_SEC);
    -distance.signum() * speed * elapsed_secs.min(AUTO_SCROLL_MAX_FRAME_SECS)
}

/// Where one auto-scroll frame lands, and whether the gesture is exhausted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoScrollLanding {
    /// The offset to write, on GPUI's own scale.
    pub offset: f32,
    /// Whether the landing reaches the newest row and can re-arm the follow.
    pub at_end: bool,
    /// Whether the viewport actually moved. False at a boundary or without a
    /// valid measurement, which lets the window retire the frame loop.
    pub moved: bool,
}

/// Clamp one auto-scroll frame to the measured transcript (#2964).
pub fn auto_scroll_landing(offset: f32, max_offset: f32, delta: f32) -> AutoScrollLanding {
    if !offset.is_finite() || !max_offset.is_finite() || !delta.is_finite() {
        return AutoScrollLanding {
            offset,
            at_end: false,
            moved: false,
        };
    }
    let max = max_offset.max(0.0);
    let next = (offset + delta).min(0.0).max(-max);
    AutoScrollLanding {
        offset: next,
        at_end: at_bottom(scrolled_from_top(next), max),
        moved: next != offset,
    }
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

    #[test]
    fn a_page_up_leaves_the_bottom_and_does_not_give_it_back() {
        // #2959: from the bottom, a page up has to land above the end. A
        // landing that still reads as the end is re-armed by the handler and
        // the reader is snapped straight back down, which is what "page up
        // does not scroll" looked like.
        let landing = page_landing(-1000.0, 1000.0, 800.0, true);
        assert_eq!(landing.offset, -200.0);
        assert!(!landing.at_end);
    }

    #[test]
    fn a_page_down_that_reaches_the_end_gives_the_follow_back() {
        let landing = page_landing(-200.0, 1000.0, 800.0, false);
        assert_eq!(landing.offset, -1000.0);
        assert!(landing.at_end);
    }

    #[test]
    fn a_page_down_that_lands_mid_page_keeps_the_reader_where_they_are() {
        // #2698: the re-arm is for a landing on the end only, so a page down
        // that stops a screen short cannot drag a reader back down.
        let landing = page_landing(-1000.0, 4000.0, 800.0, false);
        assert_eq!(landing.offset, -1800.0);
        assert!(!landing.at_end);
    }

    #[test]
    fn a_page_never_scrolls_past_either_end() {
        let top = page_landing(0.0, 1000.0, 800.0, true);
        assert_eq!(top.offset, 0.0);
        assert!(!top.at_end);
        let bottom = page_landing(-1000.0, 1000.0, 800.0, false);
        assert_eq!(bottom.offset, -1000.0);
        assert!(bottom.at_end);
    }

    #[test]
    fn content_that_fits_has_nowhere_to_page() {
        // No scroll range means the bottom by definition, so both keys leave
        // the reader exactly where they are and cost them nothing.
        let up = page_landing(0.0, 0.0, 800.0, true);
        assert_eq!(up.offset, 0.0);
        assert!(up.at_end);
        assert!(page_landing(0.0, 0.0, 800.0, false).at_end);
    }

    #[test]
    fn an_unmeasured_viewport_turns_no_page() {
        let landing = page_landing(f32::NAN, 1000.0, 800.0, true);
        assert!(landing.offset.is_nan());
        assert!(!landing.at_end);
        let landing = page_landing(-100.0, f32::INFINITY, 800.0, true);
        assert_eq!(landing.offset, -100.0);
        assert!(!landing.at_end);
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
