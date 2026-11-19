//! The shared horizontal voice preview (#3054).
//!
//! Stored messages, picked audio files and recorded drafts all render this one
//! control. Its play button and waveform are separate hit targets, and the
//! persistent track identity keeps a drag alive across dashboard repaints.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    BorderStyle, Bounds, Div, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    SharedString, Styled, canvas, div, point, prelude::*, px, quad, rgb, size,
};

use crate::{icon, logging, palette, voice_media, voice_play};

const WAVE_HEIGHT: f32 = 28.0;
const BUTTON_SIZE: f32 = 32.0;

type Track = Rc<RefCell<Option<[f32; 4]>>>;

thread_local! {
    static TRACKS: RefCell<HashMap<String, Track>> = RefCell::new(HashMap::new());
    static DRAG: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn track(id: &str) -> Track {
    TRACKS.with(|tracks| {
        Rc::clone(
            tracks
                .borrow_mut()
                .entry(id.to_string())
                .or_insert_with(|| Rc::new(RefCell::new(None))),
        )
    })
}

fn set_drag(id: String) {
    DRAG.with(|drag| *drag.borrow_mut() = Some(id));
}

fn clear_drag(id: &str) {
    DRAG.with(|drag| {
        if drag.borrow().as_deref() == Some(id) {
            *drag.borrow_mut() = None;
        }
    });
}

fn dragging(id: &str) -> bool {
    DRAG.with(|drag| drag.borrow().as_deref() == Some(id))
}

fn fraction_at(id: &str, x: f32) -> Option<f32> {
    let track = TRACKS.with(|tracks| tracks.borrow().get(id).cloned())?;
    let bounds = *track.borrow();
    let [left, _, right, _] = bounds?;
    if right <= left {
        return None;
    }
    Some(((x - left) / (right - left)).clamp(0.0, 1.0))
}

fn seek_at(id: &str, path: &Path, x: f32) {
    let Some(fraction) = fraction_at(id, x) else {
        return;
    };
    if let Err(reason) = voice_play::seek(path, fraction) {
        logging::error(&format!("voice seek: {reason}"));
    }
}

/// One voice player. `id` distinguishes two messages that carry the same file;
/// fallback values keep a just-recorded draft useful while its decode finishes.
pub(crate) fn player(
    id: &str,
    path: &Path,
    fallback_seconds: Option<f32>,
    fallback_levels: &[f32],
) -> Div {
    let shown = palette::current();
    let visual = voice_media::visual(path);
    let state = voice_play::state(path);
    let duration = state
        .map(|state| state.duration)
        .filter(|duration| !duration.is_zero())
        .or_else(|| visual.as_ref().map(|visual| visual.duration))
        .or_else(|| voice_media::duration(path).map(Duration::from_secs))
        .or_else(|| {
            fallback_seconds
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .map(Duration::from_secs_f32)
        });
    let position = state.map(|state| state.position).unwrap_or_default();
    let position = duration.map_or(position, |duration| position.min(duration));
    let fraction = duration
        .filter(|duration| !duration.is_zero())
        .map(|duration| (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let playing = state.is_some_and(|state| state.playing);
    let peaks = visual
        .map(|visual| Arc::clone(&visual.peaks))
        .unwrap_or_else(|| Arc::new(fallback_peaks(fallback_levels)));
    let time = format!(
        "{} / {}",
        clip_length(position.as_secs()),
        duration
            .map(|duration| clip_length(duration.as_secs()))
            .unwrap_or_else(|| "--:--".to_string())
    );

    let toggle_path = path.to_path_buf();
    let toggle = div()
        .id(SharedString::from(format!("voice-play-{id}")))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(BUTTON_SIZE))
        .rounded_full()
        .bg(rgb(shown.accent))
        .cursor_pointer()
        .hover(|control| control.opacity(0.82))
        .child(if playing {
            pause_mark().into_any_element()
        } else {
            icon::play_icon(0xffffff, 0.9).into_any_element()
        })
        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        })
        .on_click(move |_event, window, _cx| {
            if let Err(reason) = voice_play::toggle(&toggle_path) {
                logging::error(&format!("voice preview: {reason}"));
            }
            window.refresh();
        });

    let wave_track = track(id);
    let painted = waveform(peaks, fraction, shown.accent, shown.track, wave_track);
    let down_id = id.to_string();
    let down_path = path.to_path_buf();
    let move_id = id.to_string();
    let move_path = path.to_path_buf();
    let release_id = id.to_string();
    let release_out_id = id.to_string();
    let wave = div()
        .id(SharedString::from(format!("voice-wave-{id}")))
        .relative()
        .w_full()
        .h(px(WAVE_HEIGHT))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                set_drag(down_id.clone());
                seek_at(&down_id, &down_path, f32::from(event.position.x));
                window.refresh();
            },
        )
        .on_mouse_move(move |event: &MouseMoveEvent, window, cx| {
            if dragging(&move_id) {
                cx.stop_propagation();
                seek_at(&move_id, &move_path, f32::from(event.position.x));
                window.refresh();
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            move |_event: &MouseUpEvent, window, _cx| {
                clear_drag(&release_id);
                window.refresh();
            },
        )
        .on_mouse_up_out(
            MouseButton::Left,
            move |_event: &MouseUpEvent, window, _cx| {
                clear_drag(&release_out_id);
                window.refresh();
            },
        )
        .child(painted);

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(toggle)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(wave)
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(shown.text_muted))
                        .child(SharedString::from(time)),
                ),
        )
}

fn pause_mark() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(3.0))
        .child(
            div()
                .w(px(3.0))
                .h(px(12.0))
                .rounded(px(1.0))
                .bg(rgb(0xffffff)),
        )
        .child(
            div()
                .w(px(3.0))
                .h(px(12.0))
                .rounded(px(1.0))
                .bg(rgb(0xffffff)),
        )
}

fn fallback_peaks(levels: &[f32]) -> Vec<f32> {
    if levels.is_empty() {
        return vec![0.0; voice_media::WAVE_BARS];
    }
    (0..voice_media::WAVE_BARS)
        .map(|index| {
            let start = index * levels.len() / voice_media::WAVE_BARS;
            let end = ((index + 1) * levels.len() / voice_media::WAVE_BARS).max(start + 1);
            levels[start..end.min(levels.len())]
                .iter()
                .fold(0.0_f32, |peak, level| peak.max(level.clamp(0.0, 1.0)))
        })
        .collect()
}

fn waveform(
    peaks: std::sync::Arc<Vec<f32>>,
    fraction: f32,
    accent: u32,
    muted: u32,
    track: Track,
) -> impl IntoElement {
    canvas(
        |bounds, _window, _cx| bounds,
        move |bounds, _, window, _cx| {
            let x = f32::from(bounds.origin.x);
            let y = f32::from(bounds.origin.y);
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            track.borrow_mut().replace([x, y, x + width, y + height]);
            if peaks.is_empty() || width <= 0.0 {
                return;
            }
            let step = width / peaks.len() as f32;
            let bar_width = (step * 0.58).clamp(1.0, 3.0);
            for (index, peak) in peaks.iter().enumerate() {
                let bar_height = (peak.clamp(0.0, 1.0) * (height - 2.0)).max(2.0);
                let left = x + index as f32 * step + (step - bar_width) / 2.0;
                let top = y + (height - bar_height) / 2.0;
                let bounds = Bounds::new(
                    point(px(left), px(top)),
                    size(px(bar_width), px(bar_height)),
                );
                let played = (index as f32 + 0.5) / peaks.len() as f32 <= fraction;
                window.paint_quad(quad(
                    bounds,
                    px(bar_width / 2.0),
                    rgb(if played { accent } else { muted }),
                    px(0.0),
                    rgb(0x000000),
                    BorderStyle::default(),
                ));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full()
}

fn clip_length(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
