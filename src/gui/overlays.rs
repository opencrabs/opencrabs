//! The dialogs that take the window: the update prompt, the warning line, the
//! delete confirmation, the split's session picker and the seeker's topic
//! prompts. The Help Center moved to `help_view.rs` when it became four
//! section cards (#2893).
//!
//! Everything here draws from arguments. The window owns the state each one
//! reads and mints the listeners it needs, so a dialog can be built and checked
//! without a window, and the arm that picks which dialog to show stays in the
//! crate root beside the state it reads.
//!
//! Moved out of the crate root (#2817 follow-up) as pure code motion: the
//! functions, the two types they hand back, their comments and the constants
//! are unchanged, only the module boundary is new. `TopicPick` is `pub(crate)`
//! because the window matches on it.

use gpui::{
    AnyElement, App, ClickEvent, Div, IntoElement, SharedString, Stateful, Window, div, prelude::*,
    px, rems, rgb, rgba,
};
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::text_field::TextField;
use opencrabs::ui_core::split::Layout;

use crate::palette;
use crate::sessions;
use crate::topic_icons;

/// The update dialog (#2406). The first stage asks whether to update by
/// running /evolve; the second stage asks again before the download starts.
pub(crate) fn update_prompt_overlay(
    version: &str,
    second: bool,
    confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (title, body, yes) = if second {
        (
            "Run /evolve now?",
            "The release is downloaded, then OpenCrabs needs a restart.",
            "run",
        )
    } else {
        (
            "Update to the new version?",
            "Running /evolve downloads the latest release.",
            "continue",
        )
    };
    let panel = div()
        .id("update-prompt-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_3()
        .w(rems(26.0))
        .p_4()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(format!("v{version} is available"))),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(body)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(
                    div()
                        .id("update-prompt-cancel")
                        .px_3()
                        .py_1()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(palette::current().text_muted))
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(cancel)
                        .child(SharedString::from("cancel")),
                )
                .child(
                    div()
                        .id("update-prompt-yes")
                        .px_3()
                        .py_1()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(palette::current().accent))
                        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                        .on_click(confirm)
                        .child(SharedString::from(yes)),
                ),
        );
    div()
        .id("update-prompt-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .child(panel)
}

/// The dim over the chat and the card in its bottom right while a split waits
/// for its second session (#2438). The scrim starts past the sidebar, so the
/// rows stay bright and clickable.
pub(crate) fn split_pick_overlay(layout: Layout, rail_px: gpui::Pixels) -> impl IntoElement {
    let card = div()
        .absolute()
        .bottom(px(24.0))
        .right(px(24.0))
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from(format!(
                    "Split {}: select a session to split with",
                    layout.label()
                ))),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_faint))
                .child("or press N for a new session · esc cancels"),
        );
    div()
        .id("split-pick-scrim")
        .occlude()
        .absolute()
        .top_0()
        .bottom_0()
        .right_0()
        .left(rail_px)
        .bg(rgba(0x0a0a0a88))
        .child(card)
}

/// The icon a topic gets when none is picked (#2414, #2824).
///
/// A real badge from the pack rather than the bare speech-balloon glyph it
/// used to be: the picker offers the whole set now, so a topic created
/// without a pick still looks like one of them.
pub(crate) const DEFAULT_TOPIC_ICON: &str = "topic:opencrabs-topic-general";

/// The name prompt for a topic (#2398, #2414): a centred card over a dark scrim.
/// The card holds the name field with the chosen icon before it, and the whole
/// icon pack to pick from, one captioned family after another (#2824). Enter
/// creates, Esc or a click outside cancels.
///
/// The name arrives as the window's own [`TextField`], not a `String`: the
/// prompt paints a caret where the next character lands and a wash over the
/// selection, so the chords the composer answers show what they did (#2892).
pub(crate) fn topic_prompt_overlay(
    label: &str,
    draft: &TextField,
    icon: &str,
    families: Vec<(&'static str, Vec<Stateful<Div>>)>,
    custom_picture: Option<AnyElement>,
    pick_image: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // `render_spans` splits at char boundaries in the core, so the window
    // never slices a string and never does UTF-8 math. With no selection the
    // middle slice is empty and the caret is the only mark; with one, the
    // wash covers the selection and the caret still sits at its live end.
    let (before, selected, after) = draft.render_spans();
    let field = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .px_3()
        .py_2()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().accent))
        .bg(glass_fill(palette::current().bg))
        .text_color(rgb(palette::current().text))
        .child(SharedString::from(before.to_string()))
        .when(!selected.is_empty(), |d| {
            d.child(
                div()
                    .bg(rgb(palette::current().selection))
                    .child(SharedString::from(selected.to_string())),
            )
        })
        .child(
            div()
                .flex_none()
                .w(px(1.5))
                .h(rems(1.1))
                .bg(rgb(palette::current().accent)),
        )
        .child(SharedString::from(after.to_string()));
    // The chosen icon, as a picture when it names an embedded one and as the
    // stored glyph when the row predates the picture set (#2478). A custom
    // image arrives already built from the view's cache (#2790); an img:
    // reference never prints as a label, even when its file is missing.
    let picture = custom_picture.or_else(|| {
        topic_icons::topic_icon(icon)
            .or_else(|| topic_icons::crab_icon(icon))
            .map(|bytes| topic_icons::picture(bytes, 3.2).into_any_element())
    });
    // The badge is the session's own image, so it doubles as the upload
    // trigger: clicking it opens the same file picker the add-image cell
    // opens (#2790). The picture itself is the button, Telegram style.
    let icon_badge = div()
        .id("topic-prompt-image")
        .flex_none()
        // The badge carries the same picture the picker showed, so it has to
        // be big enough to recognise (#2768, #2789).
        .size(rems(4.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().bg))
        .text_size(rems(1.4))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .on_click(pick_image)
        .when(
            picture.is_none() && !icon.starts_with(topic_icons::IMG_PREFIX),
            |d| d.child(SharedString::from(icon.to_string())),
        )
        .children(picture);
    // The pack is 110 pictures, so the grid scrolls inside a fixed height
    // instead of growing the card past the window (#2824). Each family keeps
    // its caption: that is what makes the pack readable rather than one long
    // wall of crabs.
    let picker = div()
        .id("topic-prompt-picker")
        .flex()
        .flex_col()
        .gap_2()
        .max_h(rems(18.0))
        .overflow_y_scroll()
        .children(families.into_iter().map(|(caption, cells)| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_faint))
                        .child(SharedString::from(caption)),
                )
                .child(div().flex().flex_row().flex_wrap().gap_1().children(cells))
        }));
    let panel = div()
        .id("topic-prompt-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_3()
        .w(rems(30.0))
        .p_4()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(icon_badge)
                .child(field),
        )
        .child(picker)
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_faint))
                .child("enter creates · esc cancels"),
        );
    div()
        .id("topic-prompt-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .on_click(cancel)
        .child(panel)
}

/// The topic info panel (#2304): read only, over the window. A click on the
/// scrim or Esc closes it. It holds no editable field.
pub(crate) fn topic_info_overlay(
    info: &sessions::TopicInfo,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let created = chrono::DateTime::<chrono::Utc>::from_timestamp(info.created_at, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let parent = info
        .parent_title
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let line = |label: &'static str, value: String| {
        div()
            .flex()
            .flex_row()
            .gap_3()
            .text_sm()
            .child(
                div()
                    .w(rems(8.0))
                    .text_color(rgb(palette::current().text_muted))
                    .child(label),
            )
            .child(
                div()
                    .text_color(rgb(palette::current().text))
                    .child(SharedString::from(value)),
            )
    };
    let panel = div()
        .id("topic-info-panel")
        .occlude()
        .flex()
        .flex_col()
        .gap_2()
        .w(rems(30.0))
        .p_4()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child("topic info"),
        )
        .child(line("id", info.id.to_string()))
        .child(line("parent", parent))
        .child(line("created", created))
        .child(line("messages", info.messages.to_string()));
    div()
        .id("topic-info-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .on_click(close)
        .child(panel)
}

/// What the seeker's topic pick means (#2604, #2605). The window already
/// holds both facts the answer needs, so the branch is testable without one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TopicPick {
    /// The pick named a topic, and a topic cannot hold topics.
    Warn,
    /// The conversation has topics on, so the prompt can open.
    Draft,
    /// The conversation never had topics on: write the flag, read it back,
    /// then open the prompt.
    EnableThenDraft,
}

pub(crate) fn topic_pick(is_topic: bool, topics_enabled: bool) -> TopicPick {
    if is_topic {
        TopicPick::Warn
    } else if topics_enabled {
        TopicPick::Draft
    } else {
        TopicPick::EnableThenDraft
    }
}

/// A warning the window has to say before it does anything else (#2605). The
/// header bar names the app and the kind, so a card that stopped a pick is
/// never read as a progress notice, and one button is the whole answer.
pub(crate) fn warning_overlay(
    body: &str,
    dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let p = palette::current();
    div()
        .id("warning-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .child(
            div()
                .id("warning-panel")
                .occlude()
                .flex()
                .flex_col()
                .w(rems(26.0))
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(p.panel_hi))
                .bg(glass_fill(p.panel))
                .overflow_hidden()
                .child(
                    div()
                        .id("warning-header")
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(p.panel_hi))
                        .bg(rgb(p.panel_hi))
                        .text_sm()
                        .text_color(rgb(p.warn))
                        .child(SharedString::from("OpenCrabs - Warning")),
                )
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(p.text))
                                .child(SharedString::from(body.to_string())),
                        )
                        .child(
                            div().flex().flex_row().justify_end().child(
                                div()
                                    .id("warning-ok")
                                    .px_3()
                                    .py_1()
                                    .rounded(px(4.0))
                                    .text_xs()
                                    .cursor_pointer()
                                    .text_color(rgb(p.accent))
                                    .hover(|d| d.bg(rgb(p.panel_hi)))
                                    .on_click(dismiss)
                                    .child(SharedString::from("ok")),
                            ),
                        ),
                ),
        )
}

/// The delete confirmation: a dim card over both panes naming the
/// conversation, with the erase on its own button and nothing else. Every
/// delete path routes here, so the conversation is never one mis-click or one
/// mis-swipe away from gone.
pub(crate) fn confirm_delete_overlay(
    title: String,
    confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("confirm-delete-scrim")
        // Modal: the dim scrim takes the pointer so a click meant for the
        // rail behind it cannot open or delete anything else.
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .child(
            div()
                .id("confirm-delete")
                .flex()
                .flex_col()
                .gap_3()
                .w(rems(24.0))
                .p_4()
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(glass_fill(palette::current().panel))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from("Delete this conversation?")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .truncate()
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(
                            "Its messages leave the ledger. This cannot be undone.",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("confirm-delete-cancel")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().text_normal))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .child(SharedString::from("cancel"))
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("confirm-delete-yes")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().red))
                                .cursor_pointer()
                                .border_1()
                                .border_color(rgb(palette::current().red))
                                .hover(|d| d.bg(rgb(palette::current().selection)))
                                .child(SharedString::from("delete"))
                                .on_click(confirm),
                        ),
                ),
        )
}

/// The disable-topics confirmation (#2863): a dim card naming the
/// conversation and how many topics are about to be folded into it, with the
/// fold on its own button.
///
/// It is a second click because the fold has no way back: the topics are
/// archived and their rows live in the parent from then on. The count is
/// spelled out, so nobody folds a family of six by accident.
pub(crate) fn confirm_disable_topics_overlay(
    title: String,
    topics: usize,
    confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let fold = if topics == 0 {
        // A family that was already emptied still carries the flag, so the
        // way back out is offered and has nothing to fold (#2863).
        "Topics are switched off. Nothing is folded: this conversation has no \
         live topic."
            .to_string()
    } else if topics == 1 {
        "Its 1 topic is folded into this conversation and topics are switched \
         off. This cannot be undone."
            .to_string()
    } else {
        format!(
            "Its {topics} topics are folded into this conversation, in order, \
             and topics are switched off. This cannot be undone."
        )
    };
    div()
        .id("confirm-disable-topics-scrim")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0acc))
        .child(
            div()
                .id("confirm-disable-topics")
                .flex()
                .flex_col()
                .gap_3()
                .w(rems(26.0))
                .p_4()
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(glass_fill(palette::current().panel))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette::current().text))
                        .child(SharedString::from("Disable topics?")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .truncate()
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette::current().text_muted))
                        .child(SharedString::from(fold)),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("confirm-disable-topics-cancel")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().text_normal))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .child(SharedString::from("cancel"))
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("confirm-disable-topics-yes")
                                .px_3()
                                .py_1()
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgb(palette::current().red))
                                .cursor_pointer()
                                .border_1()
                                .border_color(rgb(palette::current().red))
                                .hover(|d| d.bg(rgb(palette::current().selection)))
                                .child(SharedString::from("disable"))
                                .on_click(confirm),
                        ),
                ),
        )
}

/// The /cd picker's rows (#2527): the parent entry first when one exists,
/// then the subdirectories, dotfile directories hidden until toggled, all
/// sorted by name. A free function so the tests can exercise the real
/// listing without a live window, the same shape the terminal's dialog
/// builds in `open_directory_picker`.
pub(crate) fn cd_entries_for(dir: &std::path::Path, show_hidden: bool) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if dir.parent().is_some() {
        files.push(dir.join(".."));
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false);
            if hidden && !show_hidden {
                continue;
            }
            files.push(path);
        }
    }
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    files
}
