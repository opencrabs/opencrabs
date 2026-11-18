//! Middle-click transcript auto-scroll (#2964).
//!
//! The speed and landing rules execute without a window. The GUI has no
//! headless mouse harness, so the remaining checks pin the real event wiring.

use crate::ui_core::scroll::{
    AUTO_SCROLL_MAX_SPEED_PX_PER_SEC, auto_scroll_delta, auto_scroll_landing,
};

const MAIN: &str = include_str!("../gui/main.rs");
const MIDDLE_SCROLL: &str = include_str!("../gui/middle_scroll.rs");
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

#[test]
fn the_dead_zone_holds_the_transcript_still() {
    assert_eq!(auto_scroll_delta(100.0, 100.0, 1.0 / 60.0), 0.0);
    assert_eq!(auto_scroll_delta(100.0, 90.0, 1.0 / 60.0), 0.0);
    assert_eq!(auto_scroll_delta(100.0, 110.0, 1.0 / 60.0), 0.0);
}

#[test]
fn pointer_sides_choose_the_reading_direction() {
    // Above the anchor moves the content down, so older rows enter from above.
    assert!(auto_scroll_delta(100.0, 50.0, 0.1) > 0.0);
    // Below the anchor moves the content up, toward newer rows.
    assert!(auto_scroll_delta(100.0, 150.0, 0.1) < 0.0);
}

#[test]
fn distance_grows_speed_and_then_caps_it() {
    let near = auto_scroll_delta(0.0, 40.0, 0.1).abs();
    let far = auto_scroll_delta(0.0, 160.0, 0.1).abs();
    assert!(far > near, "more pointer travel must scroll faster");
    let capped = AUTO_SCROLL_MAX_SPEED_PX_PER_SEC * 0.1;
    assert_eq!(auto_scroll_delta(0.0, 1000.0, 0.1).abs(), capped);
    assert_eq!(auto_scroll_delta(0.0, 10000.0, 0.1).abs(), capped);
}

#[test]
fn a_delayed_frame_cannot_jump_by_all_of_its_wall_time() {
    assert_eq!(
        auto_scroll_delta(0.0, 100.0, 10.0),
        auto_scroll_delta(0.0, 100.0, 0.1)
    );
    assert_eq!(auto_scroll_delta(0.0, 100.0, 0.0), 0.0);
    assert_eq!(auto_scroll_delta(0.0, 100.0, -1.0), 0.0);
}

#[test]
fn invalid_pointer_and_time_values_never_move() {
    assert_eq!(auto_scroll_delta(f32::NAN, 0.0, 0.1), 0.0);
    assert_eq!(auto_scroll_delta(0.0, f32::INFINITY, 0.1), 0.0);
    assert_eq!(auto_scroll_delta(0.0, 100.0, f32::NAN), 0.0);
}

#[test]
fn auto_scroll_clamps_at_the_top_without_rearming_follow() {
    let landing = auto_scroll_landing(-1000.0, 1000.0, 1200.0);
    assert_eq!(landing.offset, 0.0);
    assert!(landing.moved);
    assert!(!landing.at_end);
}

#[test]
fn reaching_the_bottom_clamps_and_rearms_follow() {
    let landing = auto_scroll_landing(-100.0, 1000.0, -1200.0);
    assert_eq!(landing.offset, -1000.0);
    assert!(landing.moved);
    assert!(landing.at_end);
}

#[test]
fn an_exhausted_boundary_retires_the_gesture() {
    let top = auto_scroll_landing(0.0, 1000.0, 100.0);
    assert!(!top.moved);
    assert!(!top.at_end);
    let bottom = auto_scroll_landing(-1000.0, 1000.0, -100.0);
    assert!(!bottom.moved);
    assert!(bottom.at_end);
    let unmeasured = auto_scroll_landing(-10.0, f32::NAN, -100.0);
    assert_eq!(unmeasured.offset, -10.0);
    assert!(!unmeasured.moved);
}

#[test]
fn the_shared_transcript_owns_the_middle_mouse_hook() {
    assert!(TRANSCRIPT.contains("pub middle_click: Option<PressHandler>"));
    assert!(TRANSCRIPT.contains(".on_mouse_down(MouseButton::Middle"));
    assert!(TRANSCRIPT.contains("middle_click(ev, window, cx);"));
    assert!(
        MAIN.contains("MiddleScrollTarget::Chat(session)"),
        "the primary transcript targets its own session"
    );
    assert!(
        PANE_VIEW.contains("MiddleScrollTarget::Pane(session)"),
        "a split transcript targets its own leaf"
    );
    assert!(MAIN.contains("middle_click: Some(middle_click)"));
    assert!(PANE_VIEW.contains("middle_click: Some(middle_click)"));
}

#[test]
fn stationary_scrolling_has_one_pending_animation_frame() {
    assert!(MAIN.contains("self.tick_middle_scroll(cx);"));
    assert!(MAIN.contains("self.schedule_middle_scroll_frame(window, cx);"));
    assert!(MIDDLE_SCROLL.contains("window.on_next_frame("));
    assert!(MIDDLE_SCROLL.contains("`Window::request_animation_frame`"));
    assert!(MIDDLE_SCROLL.contains("if !animate || self.middle_scroll_frame_pending"));
    assert!(MIDDLE_SCROLL.contains("this.middle_scroll_frame_pending = false;"));
    assert!(MIDDLE_SCROLL.contains("now.duration_since(gesture.last_tick)"));
    assert!(
        !MAIN.contains("self.middle_scroll_frame_pending = false;"),
        "an unrelated repaint must not clear the outstanding frame receipt"
    );
}

#[test]
fn every_exit_and_the_follow_landing_stay_wired() {
    let keys = include_str!("../gui/dashboard_keys.rs");
    assert!(keys.contains("self.cancel_middle_scroll(cx);"));
    assert!(MAIN.contains(".capture_key_down("));
    assert!(MAIN.contains(".capture_any_mouse_down("));
    assert!(MAIN.contains("this.move_middle_scroll(ev.position, cx);"));
    assert!(MAIN.contains("this.cancel_middle_scroll(cx);"));
    assert!(
        MIDDLE_SCROLL.contains("self.screen == Screen::Chat && self.chat_session == Some(session)")
    );
    assert!(
        MIDDLE_SCROLL
            .contains("self.screen == Screen::Chat && self.pane_states.contains_key(&session)")
    );
    assert!(MIDDLE_SCROLL.contains("self.preview.is_some() && self.preview_document.is_some()"));
    assert!(MIDDLE_SCROLL.contains("if !target_visible {"));
    assert!(MIDDLE_SCROLL.contains("follow.release();"));
    assert!(MIDDLE_SCROLL.contains("follow.rearm();"));
}

#[test]
fn upward_departure_does_not_fight_bottom_follow_slack() {
    assert!(MAIN.contains("if !self.middle_scroll_controls_chat()"));
    assert!(MAIN.contains("if !self.middle_scroll_controls_pane(session)"));
    assert_eq!(
        MIDDLE_SCROLL
            .matches("!landing.moved || (delta < 0.0 && landing.at_end)")
            .count(),
        3,
        "chat, panes and documents retire on downward bottom arrival, not upward departure"
    );
    assert!(MIDDLE_SCROLL.contains("if delta < 0.0 && landing.at_end"));
}

#[test]
fn the_marker_overlay_owns_the_pointer_it_blocks() {
    // gpui gates every mouse listener on `hitbox.is_hovered`, and the overlay's
    // `occlude()` blocks the hit test for everything under it, so a listener on
    // the window root is dead while the marker is up: the drag never reached the
    // gesture and only Esc exited (#2964). The listeners ride the overlay, the
    // one element the pointer is over.
    let overlay = MAIN
        .find("middle_scroll::overlay(gesture)")
        .expect("the marker is rendered from the window root");
    let wiring = MAIN
        .get(overlay..)
        .expect("the marker offset lands on a char boundary");
    let end = wiring
        .find("self.schedule_middle_scroll_frame(window, cx);")
        .expect("the frame schedule follows the marker");
    let wiring = wiring
        .get(..end)
        .expect("the schedule offset lands on a char boundary");
    assert!(wiring.contains(".capture_any_mouse_down("));
    assert!(wiring.contains("this.move_middle_scroll(ev.position, cx);"));
    assert!(wiring.contains(".on_scroll_wheel("));
    assert!(
        wiring.contains("cx.stop_propagation();"),
        "a press on the marker must not reach the rows it covers"
    );
    assert!(
        MIDDLE_SCROLL.contains(".occlude()"),
        "the marker keeps the transcript underneath inert"
    );
}

#[test]
fn the_anchor_is_a_fixed_circular_direction_marker() {
    assert!(MIDDLE_SCROLL.contains(".id(\"middle-scroll-overlay\")"));
    assert!(MIDDLE_SCROLL.contains(".rounded_full()"));
    assert!(MIDDLE_SCROLL.contains(".cursor(gpui::CursorStyle::None)"));
    assert!(MIDDLE_SCROLL.contains("marker_arrow(\"↑\", delta > 0.0)"));
    assert!(MIDDLE_SCROLL.contains("marker_arrow(\"↓\", delta < 0.0)"));
}

#[test]
fn the_held_middle_button_reaches_the_window() {
    // gpui's macOS view answers the middle button's press and release but not
    // its drag, and AppKit drops a drag a view has no selector for. Without
    // this the pointer never moves off the anchor and nothing scrolls (#2964).
    assert!(MIDDLE_SCROLL.contains("sel!(otherMouseDragged:)"));
    assert!(MIDDLE_SCROLL.contains("msg_send![this, mouseDragged: event]"));
    assert!(
        MIDDLE_SCROLL.contains("class.instance_method(drag).is_some()"),
        "a gpui that grows the selector itself must not be patched twice"
    );
    let opened = MAIN
        .find("failed to open the gpui window")
        .expect("the window is opened from this file");
    let installed = MAIN
        .find("middle_scroll::install_drag_bridge()")
        .expect("the gesture installs itself onto the view the window registers");
    assert!(
        installed > opened,
        "the view class only exists once the window is open"
    );
}
