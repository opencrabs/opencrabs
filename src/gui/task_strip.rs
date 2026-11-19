//! The running detached command's strip above the input.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// What a click on the task strip runs: the window mints it, the strip only
/// calls it.
pub(crate) type StripTap = Rc<dyn Fn(&mut Window, &mut App)>;

/// The running detached command, above the input.
///
/// Same formatter as the TUI (`tui::render::background`), so the two surfaces
/// cannot disagree about what a running task looks like. No border and no
/// percentage width: it sits in the column flow and disappears when idle. It
/// aligns to the chat's right edge and prints in the palette's cyan, which is
/// where the owner reads a background job (#2617).
///
/// The right alignment is not the whole row: the label keeps a gutter and a
/// clip, so in a narrow pane it stops at the pane's padding instead of
/// touching the border, and a long command ellipsizes instead of painting
/// over the neighbouring leaf (#2622).
///
/// A plain click, the press and release the drag test below lets through,
/// opens the session's task list. `on_tap` is that click, minted by the
/// window, and `None` leaves the strip a label.
pub(crate) fn task_strip(text: String, on_tap: Option<StripTap>) -> Div {
    // #2678: the roster above the composer is the line the user quotes back at
    // the agent ("that job is stuck"), so a drag over it copies it. The strip
    // is one truncated line with no character grid of its own, so there is
    // nothing to select inside it: the gesture is the copy, and it copies the
    // whole label rather than the part that survived truncation.
    //
    // The press outlives the frame it landed in. The strip is rebuilt on every
    // render, and while a turn streams or the elapsed time ticks a render
    // lands between most presses and their release: a press kept in a cell
    // minted here was gone by the time the release looked for it, so the
    // click opened the list only when no frame happened to fall in between.
    thread_local! {
        static STRIP_PRESS: std::cell::Cell<Option<Point<Pixels>>> =
            const { std::cell::Cell::new(None) };
    }
    let copied = format!("⏳ {text}");
    let clickable = on_tap.is_some();
    div()
        .when(clickable, |d| d.cursor_pointer())
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .w_full()
        .min_w_0()
        .gap_2()
        .px_2()
        .text_xs()
        .text_color(rgb(palette::current().teal))
        .on_mouse_down(MouseButton::Left, move |ev, _window, _app| {
            STRIP_PRESS.with(|press| press.set(Some(ev.position)));
        })
        .on_mouse_up(MouseButton::Left, move |ev, window, app| {
            let Some(from) = STRIP_PRESS.with(|press| press.take()) else {
                return;
            };
            if (ev.position - from).magnitude() <= 2.0 {
                if let Some(tap) = on_tap.as_ref() {
                    tap(window, app);
                }
                return;
            }
            app.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .child(SharedString::from(format!("⏳ {text}"))),
        )
}

/// One open task's output as the window last read it.
pub(crate) struct TaskTail {
    pub(crate) read_at: std::time::Instant,
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) text: Option<SharedString>,
}
