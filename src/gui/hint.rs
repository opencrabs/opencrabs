//! The hover hint the two microphones share (#3044).
//!
//! gpui builds a tooltip from any view, so one small view carries the line for
//! both surfaces: the composer's microphone and the Seeker's. It reads the same
//! gate the controls read, so a window with no speech engine is pointed at the
//! command that sets one up instead of a chord that would only print an error.
//!
//! The chord is spelled the way the composer's placeholder spells it, because
//! the two things a person reads about recording should agree with each other.
//! The Help Center's table prints the binding's own form (`ctrl-alt-r`); this
//! is the same key written the way a key is written in a sentence, and both
//! dispatch the same action.

use gpui::{
    AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px, rgb,
};

use crate::composer_view;
use crate::palette;

/// What a microphone says on hover.
///
/// The configured case names the chord and what the same key does on the
/// second press, because a person who hovers before recording is asking two
/// things: how do I start, and how do I stop.
pub(crate) fn mic_line() -> SharedString {
    if composer_view::voice_ready() {
        SharedString::from("ctrl+alt+r · record a message, same key to stop")
    } else {
        SharedString::from("/voice sets up the microphone")
    }
}

/// The tooltip's own body: one quiet line in the window's own colours.
///
/// Deliberately not a second modal: it is a hint, so it is the faint ink on a
/// raised panel and nothing else. The theme decides both, so a light window
/// draws a light hint.
struct Hint {
    text: SharedString,
}

impl Render for Hint {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = palette::current();
        div()
            .px_2()
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.track))
            .bg(rgb(theme.panel_hi))
            .text_size(px(11.0))
            .text_color(rgb(theme.text_normal))
            .child(self.text.clone())
    }
}

/// The tooltip a microphone's hover draws.
///
/// Built inside the tooltip callback, which is the only place gpui hands over
/// an `App`: the view is created on the first hover and dropped when the mouse
/// leaves, so no entity outlives the hint.
pub(crate) fn mic_tooltip(text: SharedString, cx: &mut App) -> AnyView {
    AnyView::from(cx.new(|_| Hint { text }))
}
