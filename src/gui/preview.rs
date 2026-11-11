//! Attachment preview: what a tile shows when you open it.
//!
//! The transcript draws a tile for every file a message carries, and until
//! #2395 a tile was a dead square: you could see a thumbnail but not read it,
//! play it, or find the file it came from. This module is the other half of
//! that row. It decides what a path is, what opening it means on this
//! platform, and what the overlay says about it. The pure parts live here so
//! they are tests instead of screenshots; the drawing lives in `overlay` and
//! takes the pieces it needs from the caller.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, IntoElement, ObjectFit, ScrollWheelEvent, SharedString, Window,
    div, img, px, rgb, rgba,
};

use crate::attachment_view;
use crate::logging;
use crate::palette;

/// What opening a file shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// Decoded and drawn in the overlay, zoomable.
    Image,
    /// A poster frame with a play handoff to the system player.
    Video,
    /// Name, kind, size, mtime, and the two file actions.
    File,
}

/// The extensions the strip treats as an image. Kept beside the marker
/// prefixes the send path writes (`<<IMG:...>>`) so one list decides both.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff"];

/// The extensions treated as a video. `Attachment::is_video` in the editor
/// uses a wider set (the send path knows more containers than the preview can
/// poster), but the overlay only needs to agree with the composer about what
/// is playable, and the composer's list is the one the user sees.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "webm", "mkv", "avi", "ogv", "mpg", "mpeg",
];

/// One file, opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub path: PathBuf,
    pub kind: PreviewKind,
}

impl Preview {
    /// What the primary action says. A clip with an "open" button reads like a
    /// document; "play" says what happens.
    pub fn open_label(&self) -> &'static str {
        match self.kind {
            PreviewKind::Video => "play",
            PreviewKind::Image | PreviewKind::File => "open",
        }
    }

    /// Open `path`, deciding the kind from its extension.
    pub fn of(path: PathBuf) -> Self {
        let kind = kind_of(&path);
        Self { path, kind }
    }

    /// The file name, which is what every surface calls the file: the tile
    /// caption, the overlay title, and the error when it is gone.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// The lowercased extension, for the metadata line. Empty when the file
    /// has no extension at all, which is not an error: it is just a file.
    pub fn extension(&self) -> String {
        self.path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    }

    /// The extension as the metadata line prints it, with a stand-in when the
    /// file has none: an empty value would read as a broken row.
    pub fn extension_or_unknown(&self) -> String {
        let ext = self.extension();
        if ext.is_empty() {
            "(none)".to_string()
        } else {
            ext
        }
    }
}

/// What kind of preview a path gets. Unknown extensions are files: guessing
/// wrong and decoding a document as an image would show an error instead of
/// the file the user asked for.
pub fn kind_of(path: &Path) -> PreviewKind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        PreviewKind::Image
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        PreviewKind::Video
    } else {
        PreviewKind::File
    }
}

/// The command that opens a file in whatever the system uses for it.
pub fn open_command(path: &Path) -> Vec<String> {
    let target = path.to_string_lossy().to_string();
    if cfg!(target_os = "macos") {
        vec!["open".to_string(), target]
    } else {
        vec!["xdg-open".to_string(), target]
    }
}

/// The command that opens a URL in the default browser. Same program as a
/// file: the system handler decides, we only hand it a web address instead of
/// a path. Kept next to `open_command` so the two cannot diverge.
pub fn url_command(url: &str) -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec!["open".to_string(), url.to_string()]
    } else {
        vec!["xdg-open".to_string(), url.to_string()]
    }
}

/// The command that shows a file in its folder with the file selected.
pub fn reveal_command(path: &Path) -> Vec<String> {
    let target = path.to_string_lossy().to_string();
    if cfg!(target_os = "macos") {
        vec!["open".to_string(), "-R".to_string(), target]
    } else {
        // No portable "reveal": the folder is the closest honest equivalent.
        let parent = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(target);
        vec!["xdg-open".to_string(), parent]
    }
}

/// A byte count a person can read. Binary units, one decimal below ten so
/// `1.5 MB` does not read as `2 MB`, and no decimal at all above it.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// What the overlay can say about a document on disk.
///
/// A file is not something we can draw, so the overlay states facts instead:
/// what it is, how big, when it changed, where it lives. Every field is
/// optional because a file that moved has no size to report, and saying
/// `unknown` beats drawing a zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileFacts {
    /// Size in bytes, when the file could be read.
    pub bytes: Option<u64>,
    /// Last modification, in epoch seconds.
    pub modified: Option<u64>,
}

impl FileFacts {
    /// One `stat`. Cheap enough to run while painting a document preview,
    /// unlike a subprocess, and it is the only read the document body does.
    pub fn probe(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => Self {
                bytes: Some(meta.len()),
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since| since.as_secs()),
            },
            // Gone and unreadable are the same statement here: we cannot back
            // up a number, so we do not print one.
            Err(_) => Self::default(),
        }
    }

    /// The one line a small tile can afford. A tile has no room for a labelled
    /// table, so this is the two facts that decide whether a file matters:
    /// how big it is, and the day it last changed. `None` when the file could
    /// not be read, so the tile draws its glyph alone instead of claiming
    /// `0 B` about a document it never saw.
    pub fn tile_caption(&self) -> Option<String> {
        let size = human_size(self.bytes?);
        match self.modified {
            Some(secs) => {
                let stamp = format_epoch(secs);
                let day = stamp.split(' ').next().unwrap_or(&stamp);
                Some(format!("{size} \u{b7} {day}"))
            }
            None => Some(size),
        }
    }

    /// The labelled lines under the name, in the order a reader asks them.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = vec![(
            "size",
            self.bytes
                .map(human_size)
                .unwrap_or_else(|| "unknown".to_string()),
        )];
        if let Some(secs) = self.modified {
            rows.push(("modified", format_epoch(secs)));
        }
        rows
    }
}

/// What the overlay can show for a clip before it plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poster {
    /// A frame already extracted and cached.
    Frame(PathBuf),
    /// No frame: no `ffmpeg`, a file that moved, or an extraction that failed.
    /// The overlay draws the kind glyph instead, so a preview never depends on
    /// a tool the machine may not have.
    Unavailable,
}

/// The clip facts the overlay prints, gathered ONCE when the preview opens.
///
/// Every field comes from a subprocess or a `stat`, which is why this is
/// probed on the click that opens the file and never while painting: a render
/// that shells out is a render that stutters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoFacts {
    /// The cached poster frame, when one could be made.
    pub poster: Option<PathBuf>,
    /// Length in seconds, when `ffprobe` could read it.
    pub duration: Option<u64>,
    /// File size in bytes.
    pub bytes: Option<u64>,
}

/// How long an extraction may hold up the click that asked for it. One frame
/// off a local file is milliseconds; the bound exists so a broken container
/// cannot freeze the window on a tool that never returns.
const EXTRACT_BUDGET: Duration = Duration::from_secs(3);

/// Whether the missing-`ffmpeg` note has been logged this run. Opening ten
/// clips on a machine without ffmpeg is one fact, not ten log lines.
static WARNED_NO_FFMPEG: AtomicBool = AtomicBool::new(false);

/// Is `program` runnable? Asking the binary itself, rather than reading
/// `which`, keeps one code path on macOS and Linux.
pub fn tool_available(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

impl VideoFacts {
    /// Read everything the overlay needs about `source`.
    ///
    /// `ffmpeg` is probed here, once, and its absence is a log line and a
    /// glyph, never an error the user has to act on: the clip still plays in
    /// the system player, which is what they wanted in the first place.
    pub fn probe(home: &Path, source: &Path) -> Self {
        let mut facts = Self::default();
        if !source.is_file() {
            return facts;
        }
        facts.bytes = std::fs::metadata(source).ok().map(|meta| meta.len());
        let ffmpeg = tool_available("ffmpeg");
        if let Poster::Frame(frame) = poster_for(home, source, ffmpeg) {
            facts.poster = Some(frame);
        }
        if facts.poster.is_some() {
            facts.duration = duration_secs(source);
        }
        facts
    }

    /// The line under the clip's name: what it is, how long, how big. Empty
    /// pieces are dropped instead of printed as "unknown".
    pub fn line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(secs) = self.duration {
            parts.push(format_duration(secs));
        }
        if let Some(bytes) = self.bytes {
            parts.push(human_size(bytes));
        }
        parts.push("plays in your video app".to_string());
        parts.join(" \u{b7} ")
    }
}

/// The poster for `source`, extracting it on first open.
///
/// `ffmpeg` is a parameter rather than a second probe so the no-tool branch is
/// a test instead of a fact about the machine that runs it.
pub fn poster_for(home: &Path, source: &Path, ffmpeg: bool) -> Poster {
    if !source.is_file() {
        return Poster::Unavailable;
    }
    if !ffmpeg {
        if !WARNED_NO_FFMPEG.swap(true, Ordering::Relaxed) {
            logging::warn(
                "preview: ffmpeg is not installed, so a clip shows its name and plays in the system player with no poster frame",
            );
        }
        return Poster::Unavailable;
    }
    let cached = attachment_view::poster_cache_path(home, source, mtime_secs(source));
    if is_usable(&cached) {
        return Poster::Frame(cached);
    }
    if extract_poster(source, &cached) && is_usable(&cached) {
        Poster::Frame(cached)
    } else {
        Poster::Unavailable
    }
}

/// A cache hit is a file with pixels in it. A zero byte poster is what a
/// killed ffmpeg leaves behind, and drawing it would show a blank frame
/// forever.
fn is_usable(frame: &Path) -> bool {
    std::fs::metadata(frame)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

fn mtime_secs(source: &Path) -> u64 {
    std::fs::metadata(source)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// The ffmpeg call that writes one frame, as a plain argument list so the
/// shape is a test rather than a line of shell nobody can read back.
pub fn poster_command(source: &Path, dest: &Path) -> Vec<String> {
    vec![
        "ffmpeg".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        source.display().to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-y".to_string(),
        dest.display().to_string(),
    ]
}

/// Run the extraction, bounded. Returns whether ffmpeg reported success; the
/// caller still checks the file, because exit zero and a written frame are not
/// the same statement.
fn extract_poster(source: &Path, dest: &Path) -> bool {
    if let Some(parent) = dest.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    let mut command = poster_command(source, dest);
    let program = command.remove(0);
    let spawned = std::process::Command::new(program)
        .args(&command)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return false;
    };
    let deadline = Instant::now() + EXTRACT_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                // Past the budget the frame is not coming; stop the process
                // instead of leaving it to be reaped by nobody.
                let _ = child.kill();
                let _ = child.wait();
                logging::warn("preview: ffmpeg took too long, so this clip shows no poster frame");
                return false;
            }
            Err(_) => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// Length in seconds, read with `ffprobe`. `None` when it is missing or the
/// clip does not declare a duration, which the overlay states by leaving the
/// number out rather than printing a zero.
pub fn duration_secs(source: &Path) -> Option<u64> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(source)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_duration(&String::from_utf8_lossy(&output.stdout))
}

/// `ffprobe` prints seconds with a fraction: `92.417000`. Anything that is not
/// a positive finite number is no duration at all.
pub fn parse_duration(text: &str) -> Option<u64> {
    let value: f64 = text.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    Some(value.round() as u64)
}

/// `0:07`, `1:23`, `12:05`, `1:02:03`. Minutes and seconds, because that is
/// how a player shows them.
pub fn format_duration(total: u64) -> String {
    let (hours, rest) = (total / 3600, total % 3600);
    let (minutes, seconds) = (rest / 60, rest % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Run a system command, reporting the failure instead of swallowing it.
///
/// `open` and `open -R` are the two actions on every preview, and both can
/// fail on a machine with no handler for the file. A failed spawn is a log
/// line, never a panic: the preview stays up and the file stays where it is.
pub fn launch(command: &[String]) {
    let Some((program, args)) = command.split_first() else {
        return;
    };
    match std::process::Command::new(program).args(args).spawn() {
        Ok(_) => logging::info(&format!("preview: launched {}", command_line(command))),
        Err(err) => logging::warn(&format!(
            "preview: could not launch {}: {err}",
            command_line(command)
        )),
    }
}

/// The two file actions, shared by all three kinds so they are always in the
/// same place: open the file in its application, or show it in its folder.
/// `can_open` is false when the file is gone: opening nothing is a dead
/// button, so reveal, which shows the folder it used to be in, is the only
/// action left.
pub fn actions_row(
    open_label: &'static str,
    can_open: bool,
    open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    reveal: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let mut row = div().flex().flex_row().justify_end().gap_2();
    if can_open {
        row = row.child(action_button("preview-open", open_label, open));
    }
    row.child(action_button("preview-reveal", "reveal", reveal))
        .into_any_element()
}

fn action_button(
    id: &str,
    label: &str,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id.to_string()))
        .px_3()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .text_xs()
        .text_color(rgb(palette::current().text_normal))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(SharedString::from(label.to_string()))
        .on_click(handler)
}

/// What the shell holds for this file. `actions` is the shared button row, so
/// the caller mints the listeners and this only decides the content.
pub fn body(
    preview: &Preview,
    actions: AnyElement,
    zoom: f32,
    wheel: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static,
    facts: Option<&VideoFacts>,
) -> AnyElement {
    let content = match preview.kind {
        PreviewKind::Image => image_body(preview, zoom, wheel),
        PreviewKind::Video => video_body(preview, facts.and_then(|f| f.poster.as_deref())),
        PreviewKind::File => file_body(preview),
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .child(content)
        .child(actions)
        .into_any_element()
}

/// The box an image is drawn in, in points. Both sides are pinned because gpui
/// derives an image's height from its own aspect ratio whenever the height is
/// left `Auto`, which is exactly how a portrait screenshot painted outside its
/// tile before #2395. `Contain` then fits the picture inside that box.
const IMAGE_BOX: (f32, f32) = (720.0, 480.0);

fn image_body(
    preview: &Preview,
    zoom: f32,
    wheel: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    if !preview.path.exists() {
        return missing_body(preview);
    }
    let zoom = if zoom.is_finite() {
        zoom.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        1.0
    };
    // The box grows with the zoom, and the card around it scrolls: a picture
    // zoomed past the window is reached by scrolling the card, not by having
    // it paint over the toolbar.
    let (w, h) = (IMAGE_BOX.0 * zoom, IMAGE_BOX.1 * zoom);
    div()
        .id("preview-image")
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(palette::current().bg))
        .rounded(px(6.0))
        .overflow_hidden()
        .cursor_pointer()
        .on_scroll_wheel(move |ev, window, cx| wheel(ev, window, cx))
        .child(
            img(preview.path.clone())
                // Both sides pinned: gpui derives an image's height from its
                // own aspect ratio when height is Auto, the bug behind #2395.
                .w(px(w))
                .h(px(h))
                // The same radius as the box. `Contain` paints inside the box
                // already, so this only matters for the one picture that fills
                // it exactly, where the arc is the only thing that can round
                // the corners (#2796).
                .rounded(px(6.0))
                .object_fit(ObjectFit::Contain),
        )
        .into_any_element()
}

/// What the header's second line says for this file at this zoom.
pub fn detail_line(preview: &Preview, zoom: f32, facts: Option<&VideoFacts>) -> String {
    match preview.kind {
        PreviewKind::Image => {
            let dims = image_dims(&preview.path);
            let percent = (zoom * 100.0).round() as i32;
            format!(
                "{} \u{b7} {percent}% \u{b7} scroll to zoom",
                dims_label(dims)
            )
        }
        PreviewKind::Video => facts.map(VideoFacts::line).unwrap_or_default(),
        PreviewKind::File => String::new(),
    }
}

fn video_body(preview: &Preview, poster: Option<&Path>) -> AnyElement {
    if !preview.path.exists() {
        return missing_body(preview);
    }
    let boxed = div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        // Both sides pinned, exactly like the image body: a poster frame is an
        // image, and an image with an `Auto` height is what painted outside
        // its row before #2395.
        .w(px(IMAGE_BOX.0))
        .h(px(IMAGE_BOX.1))
        .bg(rgb(palette::current().bg))
        .rounded(px(6.0))
        .overflow_hidden();
    match poster {
        Some(frame) => boxed
            .child(
                img(frame.to_path_buf())
                    .w(px(IMAGE_BOX.0))
                    .h(px(IMAGE_BOX.1))
                    .rounded(px(6.0))
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element(),
        None => boxed
            .child(div().text_lg().child(SharedString::from("\u{1f3ac}")))
            .child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(rgb(palette::current().text_muted))
                    .child(SharedString::from("no poster frame")),
            )
            .into_any_element(),
    }
}

fn file_body(preview: &Preview) -> AnyElement {
    let mut col = div().flex().flex_col().gap_1().w(px(IMAGE_BOX.0));
    col = col.child(row("kind", preview.extension_or_unknown()));
    for (label, value) in FileFacts::probe(&preview.path).rows() {
        col = col.child(row(label, value));
    }
    col.child(row("path", preview.path.display().to_string()))
        .into_any_element()
}

fn missing_body(preview: &Preview) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w(px(IMAGE_BOX.0))
        .p_4()
        .bg(rgb(palette::current().bg))
        .rounded(px(6.0))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text))
                .child(SharedString::from(
                    "This file is no longer where it was sent from",
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(preview.path.display().to_string())),
        )
        .into_any_element()
}

fn row(label: &str, value: String) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .child(
            div()
                .w(px(72.0))
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex_1()
                .truncate()
                .text_xs()
                .text_color(rgb(palette::current().text_normal))
                .child(SharedString::from(value)),
        )
        .into_any_element()
}

/// A UTC stamp for a modification time, `YYYY-MM-DD HH:MM`, so the overlay
/// does not depend on a locale or a timezone database.
pub fn format_epoch(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        time / 3600,
        (time % 3600) / 60
    )
}

/// Howard Hinnant's civil-from-days, the standard integer conversion from days
/// since 1970-01-01 to a calendar date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// One argument, quoted for a shell string.
///
/// The commands themselves are spawned as argv, so this is not how a path
/// survives a space: it is how a path survives being PRINTED. The log line and
/// any hint that shows the user what was run must not turn `a b.png` into two
/// words, and must not let a quote in a file name swallow the rest of the line.
pub fn quoted(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.+/=:@%,".contains(c));
    if safe {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// A whole command as one readable line, every argument quoted.
pub fn command_line(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| quoted(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The real pixel dimensions of an image, or `None` when it cannot be read.
///
/// Read once when the overlay opens, never on the render path: decoding a
/// header per frame would make the pane stutter for nothing.
pub fn image_dims(path: &Path) -> Option<(u32, u32)> {
    image::image_dimensions(path).ok()
}

/// The dimensions as the header prints them, with a stand-in when the file
/// cannot be measured (a format the decoder does not know, say).
pub fn dims_label(dims: Option<(u32, u32)>) -> String {
    match dims {
        Some((w, h)) => format!("{w} x {h}"),
        None => "size unknown".to_string(),
    }
}

/// How far one scroll notch moves the zoom.
pub const ZOOM_STEP: f32 = 0.1;
/// The closest the image may be drawn: below this the picture is a postage
/// stamp and the buttons under it are the only thing left on screen.
pub const ZOOM_MIN: f32 = 0.25;
/// The furthest in. Past this a screenshot is all artefact.
pub const ZOOM_MAX: f32 = 4.0;

/// The zoom a scroll of `lines` notches asks for, clamped.
pub fn zoom_after_scroll(zoom: f32, lines: f32) -> f32 {
    let zoom = if zoom.is_finite() { zoom } else { 1.0 };
    let lines = if lines.is_finite() { lines } else { 0.0 };
    (zoom + lines * ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// The human word for a kind, which is what the overlay's title line calls it.
pub fn kind_label(kind: PreviewKind) -> &'static str {
    match kind {
        PreviewKind::Image => "image",
        PreviewKind::Video => "video",
        PreviewKind::File => "file",
    }
}

/// The preview, drawn over everything.
///
/// The shape is the delete dialog's, because a modal that behaves differently
/// from the last modal is how muscle memory breaks: a scrim that takes the
/// pointer (`.occlude()`), the whole window covered (`.absolute()`, `size_full`),
/// and a card in the middle. Two dismissal paths, one for each reflex: the
/// scrim closes it and so does the button, while a click on the card itself is
/// inert so inspecting the picture never throws it away.
///
/// `body` is the part that differs per kind: the image, the poster, or the
/// metadata. The shell is shared so those three can never drift apart.
pub fn overlay(
    preview: &Preview,
    detail: String,
    body: AnyElement,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let close = std::rc::Rc::new(close);
    let scrim_close = close.clone();
    let button_close = close.clone();
    div()
        .id("preview-scrim")
        // Modal: the dim scrim owns the pointer, so a click meant for the
        // transcript behind it cannot fold a turn or open another file.
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(rgba(0x0a0a0add))
        .on_click(move |ev, window, cx| scrim_close(ev, window, cx))
        .child(
            div()
                .id("preview-card")
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .max_w(px(860.0))
                .max_h(px(620.0))
                // A zoomed picture is clipped to the card instead of painting
                // over the window: the card is the frame, the picture is what
                // fits in it.
                .overflow_hidden()
                .rounded(px(10.0))
                .border_1()
                .border_color(rgb(palette::current().panel_hi))
                .bg(rgb(palette::current().panel))
                // Inert: the click stops here instead of reaching the scrim,
                // so looking closely at a picture never closes it.
                .on_click(|_ev, _window, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(palette::current().text))
                                .truncate()
                                .child(SharedString::from(preview.name())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette::current().text_muted))
                                .child(SharedString::from(kind_label(preview.kind))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette::current().text_muted))
                                .child(SharedString::from(detail)),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("preview-close")
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_xs()
                                .text_color(rgb(palette::current().text_normal))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                                .child(SharedString::from("esc"))
                                .on_click(move |ev, window, cx| button_close(ev, window, cx)),
                        ),
                )
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures hang off the OS temp dir, built from `temp_dir()` rather than
    /// a `/tmp` literal: that literal is live for the Windows build and the
    /// footgun scan flags it in test code too.
    fn p(name: &str) -> PathBuf {
        std::env::temp_dir().join("gui_attach").join(name)
    }

    #[test]
    fn preview_kind_follows_the_extension_and_never_guesses() {
        assert_eq!(kind_of(&p("shot.PNG")), PreviewKind::Image);
        assert_eq!(kind_of(&p("clip.mov")), PreviewKind::Video);
        assert_eq!(kind_of(&p("report.pdf")), PreviewKind::File);
        // No extension at all is a file, not an image.
        assert_eq!(kind_of(&p("Makefile")), PreviewKind::File);
        // A trailing dot has an empty extension: still a file.
        assert_eq!(kind_of(&p("weird.")), PreviewKind::File);
    }

    #[test]
    fn a_preview_keeps_the_path_and_names_the_file() {
        let preview = Preview::of(p("screenshot.png"));
        assert_eq!(preview.kind, PreviewKind::Image);
        assert_eq!(preview.name(), "screenshot.png");
        assert_eq!(preview.extension(), "png");
        assert_eq!(preview.path, p("screenshot.png"));
    }

    #[test]
    fn open_and_reveal_hand_the_path_to_the_system() {
        let path = p("a file with spaces.png");
        let shown = path.to_string_lossy().into_owned();
        let open = open_command(&path);
        assert_eq!(open[1], shown);
        assert_eq!(
            open[0],
            if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            }
        );

        let reveal = reveal_command(&path);
        if cfg!(target_os = "macos") {
            assert_eq!(reveal[0], "open");
            assert_eq!(reveal[1], "-R");
            assert_eq!(reveal[2], shown);
        } else {
            assert_eq!(
                reveal[1],
                path.parent().unwrap().to_string_lossy().into_owned()
            );
        }
    }

    #[test]
    fn human_size_reads_at_a_glance() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(
            human_size(1023),
            "1023 B",
            "the byte boundary, not rounded up"
        );
        assert_eq!(human_size(1024), "1.0 KB", "the kilobyte boundary");
        assert_eq!(human_size(1025), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(10 * 1024), "10 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB", "the megabyte boundary");
        assert_eq!(
            human_size(1024 * 1024 * 1024),
            "1.0 GB",
            "the gigabyte boundary"
        );
        assert_eq!(human_size(1024u64.pow(4)), "1.0 TB");
        // A file that moved, or one we are not allowed to read, is not a zero
        // byte file: the overlay says it does not know.
        let gone = FileFacts::probe(&p("no-such-document.pdf"));
        assert_eq!(gone, FileFacts::default());
        assert_eq!(gone.rows(), vec![("size", "unknown".to_string())]);
    }

    #[test]
    fn a_tile_caption_states_size_and_day_and_never_guesses() {
        let real = p("caption.txt");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, vec![b'x'; 1536]).unwrap();
        let caption = FileFacts::probe(&real).tile_caption().unwrap();
        assert_eq!(caption.len(), "1.5 KB · 2026-11-02".len(), "size, dot, day");
        assert!(caption.starts_with("1.5 KB \u{b7} "), "got {caption}");
        let rows = FileFacts::probe(&real).rows();
        let stamp = &rows[1].1;
        let day = stamp.split(' ').next().unwrap_or(stamp);
        assert!(caption.ends_with(day), "got {caption}");
        std::fs::remove_file(&real).ok();
        // A file we cannot read gets no caption at all: the tile draws its
        // glyph alone rather than claiming `0 B` about a document it never saw.
        assert_eq!(
            FileFacts::probe(&p("no-such-caption.pdf")).tile_caption(),
            None
        );
    }

    /// A document that IS readable states its size and stamp, and the rows are
    /// what the overlay prints, so this is the body's content under test.
    #[test]
    fn file_facts_state_the_size_and_stamp_of_a_real_file() {
        let real = p("facts.txt");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, vec![b'x'; 1536]).unwrap();
        let facts = FileFacts::probe(&real);
        assert_eq!(facts.bytes, Some(1536));
        let rows = facts.rows();
        assert_eq!(rows[0], ("size", "1.5 KB".to_string()));
        assert_eq!(rows[1].0, "modified");
        assert_eq!(rows[1].1, format_epoch(facts.modified.unwrap()));
        assert_eq!(rows[1].1.len(), 16, "YYYY-MM-DD HH:MM, no locale guess");
        std::fs::remove_file(&real).ok();
    }

    #[test]
    fn a_clip_with_no_ffmpeg_or_no_file_falls_back_instead_of_failing() {
        let home = std::env::temp_dir().join("gui_attach").join("home");
        let home = home.as_path();
        assert_eq!(
            poster_for(home, &p("moved-away.mp4"), false),
            Poster::Unavailable,
            "a clip that moved has no frame to show"
        );
        let real = p("poster-test-clip.mp4");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, b"not really a video").unwrap();
        assert_eq!(
            poster_for(home, &real, false),
            Poster::Unavailable,
            "without ffmpeg the overlay draws a glyph and the clip still plays"
        );
        let key = attachment_view::poster_cache_path(home, &real, mtime_secs(&real));
        assert!(
            !key.exists(),
            "an extraction that never ran leaves no cache file behind"
        );
        std::fs::remove_file(&real).ok();
    }

    /// The clip line carries what it knows and drops what it does not, so a
    /// file with no readable duration is not printed as `0:00`.
    #[test]
    fn clip_line_names_duration_and_size_and_omits_what_it_lacks() {
        let known = VideoFacts {
            poster: None,
            duration: Some(83),
            bytes: Some(4_200_000),
        };
        assert_eq!(
            known.line(),
            "1:23 \u{b7} 4.0 MB \u{b7} plays in your video app"
        );
        assert_eq!(
            VideoFacts::default().line(),
            "plays in your video app",
            "a clip ffprobe could not read still says how it opens"
        );
    }

    #[test]
    fn durations_read_like_a_player_not_like_seconds() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(7), "0:07");
        assert_eq!(format_duration(83), "1:23");
        assert_eq!(format_duration(725), "12:05");
        assert_eq!(format_duration(3723), "1:02:03");
        assert_eq!(parse_duration("92.417000\n"), Some(92));
        assert_eq!(
            parse_duration("N/A"),
            None,
            "ffprobe writes N/A for no duration"
        );
        assert_eq!(
            parse_duration("0"),
            None,
            "zero is a missing duration, not a length"
        );
        assert_eq!(parse_duration("-3"), None);
    }

    /// The ffmpeg call is an argument list, and the video path stays ONE
    /// argument: a clip called `clip one.mp4` must not become two.
    #[test]
    fn poster_command_extracts_exactly_one_frame_to_the_cache_path() {
        let clip = std::env::temp_dir().join("vids").join("clip one.mp4");
        let poster = std::env::temp_dir()
            .join("home")
            .join("cache/posters/ab12.png");
        let (clip_shown, poster_shown) = (
            clip.to_string_lossy().into_owned(),
            poster.to_string_lossy().into_owned(),
        );
        let args = poster_command(&clip, &poster);
        assert_eq!(args.first().map(String::as_str), Some("ffmpeg"));
        let frames = args.iter().position(|a| a == "-frames:v").unwrap();
        assert_eq!(args.get(frames + 1).map(String::as_str), Some("1"));
        assert!(
            args.iter().any(|a| a == &clip_shown),
            "the input path is one argument, spaces and all"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some(poster_shown.as_str()),
            "the frame lands where the planner says it is cached"
        );
    }

    #[test]
    fn a_video_offers_play_and_a_document_offers_open() {
        assert_eq!(Preview::of(p("clip.mp4")).open_label(), "play");
        assert_eq!(Preview::of(p("shot.png")).open_label(), "open");
        assert_eq!(Preview::of(p("report.pdf")).open_label(), "open");
    }
    /// The shell has to be a real modal: covering the window, holding the
    /// pointer, closing from the scrim and the button, and NOT closing when
    /// the card itself is clicked. Read off the source because a click can
    /// only be dispatched by a live window, and each of these is a line that
    /// a later edit could quietly drop.
    #[test]
    fn preview_shell_is_modal_dismissable_and_inert_inside() {
        let src = include_str!("preview.rs");
        assert!(
            src.contains(".id(\"preview-scrim\")"),
            "the scrim needs an id"
        );
        assert!(src.contains(".occlude()"), "a modal takes the pointer");
        assert!(src.contains(".absolute()"), "it sits over the window");
        assert!(src.contains(".size_full()"), "and covers all of it");
        assert!(
            src.contains("scrim_close(ev, window, cx)"),
            "a click on the scrim closes the preview"
        );
        assert!(
            src.contains(".id(\"preview-close\")"),
            "there is a visible close affordance"
        );
        assert!(
            src.contains("cx.stop_propagation()"),
            "the body swallows its clicks so inspecting never closes it"
        );
    }

    /// A path with a space or a quote must survive being printed: the log line
    /// and any hint that shows the command are read by a human, and a broken
    /// quote there reads as a broken feature.
    #[test]
    fn preview_image_quotes_a_path_with_spaces_and_quotes() {
        assert_eq!(quoted("/var/media/a b.png"), "'/var/media/a b.png'");
        assert_eq!(quoted("/var/media/plain.png"), "/var/media/plain.png");
        // A single quote inside the name keeps every quote balanced.
        let awkward = quoted("/var/media/it's here.png");
        assert_eq!(awkward, "'/var/media/it'\\''s here.png'");
        // The shell-safe form survives a whole command, not just one name.
        assert_eq!(
            command_line(&["open".to_string(), "/var/media/it's here.png".to_string()]),
            "open '/var/media/it'\\''s here.png'"
        );
        // The opener is the platform's own (`open` on macOS, `xdg-open`
        // elsewhere), so the expectation follows it: what this test pins is
        // that the path survives quoting, not which binary CI happens to have.
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(
            command_line(&open_command(&PathBuf::from("/var/media/a b.png"))),
            format!("{opener} '/var/media/a b.png'")
        );
    }

    /// A missing file is a state, not a crash and not a blank box: the overlay
    /// says the file is gone and still offers reveal, because the folder it
    /// used to live in is often where the user finds out what happened.
    #[test]
    fn preview_image_reports_a_missing_file_instead_of_drawing_nothing() {
        let gone = Preview::of(p("definitely-not-here.png"));
        assert!(!gone.path.exists());
        let src = include_str!("preview.rs");
        assert!(
            src.contains("if !preview.path.exists() {\n        return missing_body(preview);"),
            "the image branch checks the file before it draws"
        );
        assert!(
            src.contains("This file is no longer where it was sent from"),
            "and says so in words, not a blank frame"
        );
        assert!(
            src.contains("if can_open {"),
            "a missing file offers reveal only, never a dead open button"
        );
        assert!(
            image_dims(&gone.path).is_none(),
            "an unreadable file has no dims"
        );
    }

    /// Zoom is bounded at both ends: a scroll cannot shrink the picture to
    /// nothing or blow it past the decoder's memory.
    #[test]
    fn preview_image_zoom_stays_within_bounds() {
        assert_eq!(zoom_after_scroll(1.0, 1.0), 1.1);
        assert_eq!(zoom_after_scroll(1.0, -1.0), 0.9);
        assert_eq!(zoom_after_scroll(1.0, 100.0), ZOOM_MAX);
        assert_eq!(zoom_after_scroll(1.0, -100.0), ZOOM_MIN);
        assert_eq!(zoom_after_scroll(f32::NAN, 0.0), 1.0);
        assert_eq!(zoom_after_scroll(1.0, f32::NAN), 1.0);
    }

    #[test]
    fn preview_image_dims_label_says_what_it_knows() {
        assert_eq!(dims_label(Some((1920, 1080))), "1920 x 1080");
        assert_eq!(dims_label(None), "size unknown");
    }
}
