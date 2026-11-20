//! The composer's voice player, drawn as the same material as the Seeker orb
//! (#3092).
//!
//! A finished take is an object, not a row of controls. The marble holds it:
//! the take's own peaks displace the silhouette, the band across the equator
//! is the scrub track, and tapping the glass plays or pauses.
//!
//! Three deliberate differences from the recording orb in `seeker_orb`:
//!
//! - there is no worker thread. `OrbAnimation` renders every 33 ms until it is
//!   dropped, which is right for a microphone and wrong for a player that sits
//!   paused for most of its life. This renders on demand;
//! - the silhouette is displaced by the take's amplitude, so the outline
//!   itself carries the waveform. The body stays glass, which is what keeps
//!   the text behind it readable;
//! - the body is thinner than the recording orb's, because a player is
//!   something you read a message through, not something you look at alone.
//!
//! Every fitted value is imported from `seeker_orb` rather than copied, so a
//! refit there reaches both surfaces. The per-pixel maths has to differ,
//! because the dynamics differ.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Div, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, RenderImage, SharedString,
    Styled, canvas, div, img, prelude::*, px,
};
use image::{Frame, RgbaImage};

use super::seeker_orb::{
    AMBER_HUE, AMBIENT, BAND_ARCH, BAND_GAIN, BAND_REACH, BAND_SIGMA, BAND_WALL, BAYER, BOW_BOWL,
    BOWL_ALPHA, BOWL_HUE, BOWL_LIFT, BREATHE, COOL_HUE, DISPERSION, DOME_ALPHA, EDGE_CAUSTIC,
    EDGE_COOL, EDGE_DEPTH, EDGE_SPREAD, EDGE_WARM, FAR_LIFT, FAR_WALL, GLOW_GAIN, GLOW_SIGMA,
    HORIZON, MAX_LIFT, ORB_RADIUS, RIM_BASE, RIM_FALL_GAIN, RIM_FALL_SIGMA, RIM_GAIN, RIM_RADIUS,
    RIM_SILVER, RIM_TILT_X, RIM_TILT_Y, RIM_TOP_EDGE, RIM_TOP_FULL, RIM_TOP_GAIN, RIM_TOP_WIDTH,
    RIM_WIDTH, SCRIM_LUMA, SIDE_BIAS, SPEC_DIR_X, SPEC_DIR_Y, SPEC_GAIN, SPEC_RADIUS,
    SPEC_SPAN_HIGH, SPEC_SPAN_LOW, SPEC_WIDTH, Surface, VOICE_SAMPLES, WALL_ALPHA, WALL_EDGE,
    WALL_FULL, WARM_FOLLOW, bgra, gaussian, hash_noise, relative_luma, rgb_hsv, saturation_at,
    sine, smooth, smooth_voice, tinted, voice_at,
};
use super::voice_play::{self, ClipState};
use super::voice_preview::{clip_length, fallback_peaks};
use crate::{logging, palette, voice_media};

/// How far the silhouette moves, in orb units, at full amplitude.
///
/// Two percent is enough to see the edge breathe and small enough that the
/// marble never stops reading as a circle. Larger values turn it into a blob.
const EDGE_SWING: f32 = 0.022;

/// The body is this much thinner than the recording orb's, so a message reads
/// through the glass while the take is held.
const BODY_TRANSMISSION: f32 = 0.62;

/// Band brightness before the playhead, as a share of the fitted band gain.
const BAND_AHEAD: f32 = 0.14;

/// Band brightness when nothing is playing: a calm, full ring.
const BAND_REST: f32 = 0.46;

/// How soft the playhead's edge is along the track, in track units.
const BAND_SOFT: f32 = 0.012;

/// The playhead's own catch, where the light meets the part already played.
const PLAYHEAD_GAIN: f32 = 0.30;
const PLAYHEAD_WIDTH: f32 = 0.010;

/// The play and pause marks, in orb units from the centre.
const GLYPH_HALF_HEIGHT: f32 = 0.26;
const GLYPH_APEX: f32 = 0.24;
const GLYPH_BASE: f32 = -0.15;
const GLYPH_BAR_INNER: f32 = 0.035;
const GLYPH_BAR_OUTER: f32 = 0.155;
const GLYPH_GAIN: f32 = 0.72;

/// How much a hovered orb leans on its rim, so the tap target answers the
/// pointer before it is clicked.
const HOVER_RIM: f32 = 0.35;

/// One instant of the player, supplied by the composer each repaint.
#[derive(Clone, Copy, Default)]
pub(crate) struct PlayerState {
    /// Where the playhead sits, nought to one. Ignored while paused.
    pub(crate) progress: f32,
    /// Whether the clip is sounding. Decides the mark and the track.
    pub(crate) playing: bool,
    /// Whether the pointer is over the orb.
    pub(crate) hovered: bool,
    /// Seconds since the player appeared, for the idle breath.
    pub(crate) clock: f32,
}

/// A finished take drawn as the orb. Owns the geometry, which is the expensive
/// part, and renders one frame per call.
pub(crate) struct PlayerOrb {
    surface: Surface,
    levels: [f32; VOICE_SAMPLES],
    colour: u32,
}

impl PlayerOrb {
    /// Build a player for one take. `peaks` is whatever the decoder produced,
    /// at whatever length, and is pooled down to the meter's own width.
    pub(crate) fn new(colour: u32, peaks: &[f32]) -> Self {
        Self::at(PLAYER_PIXELS, colour, peaks)
    }

    /// The same player at an arbitrary frame size, so the tests can render a
    /// small one instead of paying for a full frame per assertion.
    fn at(size: u32, colour: u32, peaks: &[f32]) -> Self {
        Self {
            surface: Surface::new(size),
            levels: smooth_voice(resample(peaks)),
            colour,
        }
    }

    /// Play or pause the take this orb stands for.
    ///
    /// The audio lives in `voice_play`, which owns the output stream, the seek
    /// and the position. The composer asks the orb, the orb asks the player, so
    /// nothing outside this module has to know how a clip is started.
    pub(crate) fn activate(path: &Path) -> Result<(), String> {
        voice_play::toggle(path)
    }

    /// Move the playhead to `fraction` of the take, clamped to the track.
    pub(crate) fn scrub(path: &Path, fraction: f32) -> Result<(), String> {
        voice_play::seek(path, fraction.clamp(0.0, 1.0))
    }

    /// Render one frame. Straight alpha BGRA, the layout GPUI's atlas expects.
    pub(crate) fn render(&self, state: PlayerState) -> RenderImage {
        let surface = &self.surface;
        let size = surface.size;
        let mut pixels = RgbaImage::new(size, size);
        let accent_hue = rgb_hsv(self.colour).0;
        let sweep = if state.playing {
            state.progress.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let hover = if state.hovered { HOVER_RIM } else { 0.0 };
        let breathe = if state.playing {
            BREATHE * (sine(state.clock * 0.70) - 0.5)
        } else {
            0.0
        };
        // One voice lookup per column instead of per pixel: the edge shift only
        // depends on where across the orb a pixel sits.
        let mut swing = vec![0.0_f32; size as usize];
        for (column, shift) in swing.iter_mut().enumerate() {
            let along = (column as f32 + 0.5) / size as f32;
            *shift = EDGE_SWING * (voice_at(&self.levels, along) - 0.5) * 2.0;
        }
        let feather = 1.5 / (ORB_RADIUS * size as f32 * 0.5);

        for (index, pixel) in pixels.pixels_mut().enumerate() {
            let radius = surface.radius[index];
            let x = surface.x[index];
            let y = surface.y[index];
            let nx = x / ORB_RADIUS;
            let ny = y / ORB_RADIUS;
            let nz = surface.normal_z[index];
            let column = index as u32 % size;

            // The silhouette, moved by the take. `depth` is how far inside the
            // displaced edge this pixel sits, which is the coordinate the rim
            // and the shoulder need: both live ON the edge.
            let reach = 1.0 + swing[column as usize];
            let depth = (reach - radius).max(0.0);
            // Clamped BEFORE it meets the glass. Multiplying an unclamped ramp
            // into the density pins every inner pixel to fully opaque, which is
            // how the recording orb once lost its transmission entirely.
            let edge = (depth * ORB_RADIUS * size as f32 * 0.5).clamp(0.0, 1.0);
            if edge <= 0.0 {
                continue;
            }

            // The seam between the cool dome and the warm bowl, and the band of
            // light that rides it. Same geometry as the recording orb, with the
            // voice-reactive terms left out: a player is not listening.
            let horizon = HORIZON + BOW_BOWL * (1.0 - nz) + breathe;
            let seam = horizon - BAND_ARCH * (1.0 - nz);
            let offset = ny - seam;
            let lateral = BAND_WALL + (1.0 - BAND_WALL) * gaussian(nx, BAND_REACH);
            let band_shape = gaussian(offset, BAND_SIGMA) * lateral;
            let glow = gaussian(offset, GLOW_SIGMA) * (1.0 + SIDE_BIAS * -nx) * lateral;
            let bowl = smooth(horizon, horizon + 0.60, ny);
            let dome = smooth(-0.04, 0.04, -ny);

            // The mark: a play triangle while paused, two bars while playing.
            // It sits at the centre and punches through the band, so the light
            // frames it instead of washing it out.
            let mark = if state.playing {
                pause_mark(nx, ny, feather)
            } else {
                play_mark(nx, ny, feather)
            };

            // The track: everything behind the playhead is lit, everything
            // ahead is a dim rail. While paused the whole ring is calm and even.
            let along = (nx * 0.5 + 0.5).clamp(0.0, 1.0);
            let behind = smooth(-BAND_SOFT, BAND_SOFT, sweep - along);
            let track = if state.playing {
                BAND_AHEAD + (1.0 - BAND_AHEAD) * behind
            } else {
                BAND_REST
            };
            let head = if state.playing {
                PLAYHEAD_GAIN
                    * gaussian(along - sweep, PLAYHEAD_WIDTH)
                    * gaussian(offset, BAND_SIGMA * 1.6)
            } else {
                0.0
            };

            // The wall caustic: the narrow chromatic band just inside the rim,
            // cool above the seam and warm below.
            let edge_band = gaussian(radius - EDGE_DEPTH, EDGE_SPREAD) * nx * nx;
            let edge_gain = EDGE_WARM + (EDGE_COOL - EDGE_WARM) * dome;
            let caustic = edge_band * edge_gain;

            // The border, placed by distance from the displaced edge so the
            // ring rides the moving silhouette instead of a fixed circle.
            let inset = 1.0 - RIM_RADIUS;
            let rim_key = (0.5 - 0.5 * (nx * RIM_TILT_X + ny * RIM_TILT_Y)).max(0.0);
            let rim_ring = gaussian(depth - inset, RIM_WIDTH);
            let rim_arc = smooth(RIM_TOP_EDGE, RIM_TOP_FULL, ny);
            let rim_top = gaussian(depth - inset, RIM_TOP_WIDTH) * rim_arc;
            let spec_radius = (nx * nx + ny * ny).sqrt().max(1e-4);
            let spec_bearing = (nx * SPEC_DIR_X + ny * SPEC_DIR_Y) / spec_radius;
            let catch = gaussian(spec_radius - SPEC_RADIUS, SPEC_WIDTH)
                * smooth(SPEC_SPAN_LOW, SPEC_SPAN_HIGH, spec_bearing);

            // The mark is a hole in the light, not a shape painted on top of
            // it: the band and its halo step aside so the white triangle is not
            // swallowed by the brightest thing in the material.
            let lit = 1.0 - mark;
            let heat = (AMBIENT
                + BAND_GAIN * band_shape * track * lit
                + GLOW_GAIN * glow * lit
                + BOWL_LIFT * bowl
                + RIM_FALL_GAIN * (-depth / RIM_FALL_SIGMA).exp()
                + RIM_GAIN * rim_ring * (RIM_BASE + (1.0 - RIM_BASE) * rim_key) * (1.0 + hover)
                + RIM_TOP_GAIN * rim_top
                + FAR_WALL * gaussian(ny - (horizon + FAR_LIFT), GLOW_SIGMA * 0.85)
                + SPEC_GAIN * catch
                + caustic
                + head
                + GLYPH_GAIN * mark)
                .clamp(0.0, 1.0);

            let base_saturation = saturation_at(heat);
            let saturation =
                base_saturation + (EDGE_CAUSTIC - base_saturation) * edge_band.min(1.0);
            let fringe = DISPERSION * depth.powi(6);
            let warm_hue = BOWL_HUE + (accent_hue - AMBER_HUE) * WARM_FOLLOW;
            let warm = tinted(warm_hue - fringe, saturation, heat);
            let cool = tinted(COOL_HUE + fringe, saturation, heat);
            let mut red = warm.0 * (1.0 - dome) + cool.0 * dome;
            let mut green = warm.1 * (1.0 - dome) + cool.1 * dome;
            let mut blue = warm.2 * (1.0 - dome) + cool.2 * dome;
            // The mark and the overhead catch lose their hue: a play button is
            // white, not a lit cyan shape.
            let silver = (RIM_SILVER * rim_top + mark).clamp(0.0, 1.0);
            red += (1.0 - red) * silver;
            green += (1.0 - green) * silver;
            blue += (1.0 - blue) * silver;

            // Density from geometry, thinner than the recording orb's. The mark
            // is the one part of the player that is solid, so it stays legible
            // over whatever the glass is sitting on.
            let region = smooth(-0.15, 0.25, -ny);
            let deep = BOWL_ALPHA + (DOME_ALPHA - BOWL_ALPHA) * region;
            let chord = 0.55 + 0.45 * nz;
            let wall = nx * nx * smooth(WALL_FULL, WALL_EDGE, radius);
            let density = ((deep * chord * (1.0 - wall) + WALL_ALPHA * wall) * BODY_TRANSMISSION)
                .max(mark * 0.88)
                .clamp(0.0, 1.0);

            // The fitted values are the reference's composited pixels, so over
            // the dark scrim the emitted colour is lifted to land on them.
            if density > 1e-4 {
                let emitted = relative_luma(red, green, blue);
                if emitted > 1e-4 {
                    let composited = (heat - SCRIM_LUMA * (1.0 - density)) / density;
                    let gain = (composited.max(0.0) / emitted).min(MAX_LIFT);
                    red = (red * gain).min(1.0);
                    green = (green * gain).min(1.0);
                    blue = (blue * gain).min(1.0);
                }
            }

            let alpha = (density * edge).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }
            let row = index as u32 / size;
            let dither = ((BAYER[((row & 3) * 4 + (column & 3)) as usize] - 0.5) * 1.4
                + (hash_noise(index) - 0.5) * 0.8)
                / 255.0;
            *pixel = bgra(red + dither, green + dither, blue + dither, alpha + dither);
        }
        RenderImage::new(vec![Frame::new(pixels)])
    }
}

/// The size the composer's player is rasterised at, in pixels.
///
/// The orb is drawn at sixty-four points, so this is three times its on-screen
/// size: enough for a retina display without paying the recording orb's full
/// frame every time the playhead moves a hair.
const PLAYER_PIXELS: u32 = 192;

/// The size the player is drawn at, in points.
const ORB_POINTS: f32 = 64.0;

/// How many steps of the track the playhead takes before the frame is redrawn.
///
/// The band is about sixty points wide, so one step in a hundred and
/// twenty-eight is half a point: invisible, and it keeps a repaint every forty
/// milliseconds from re-rasterising the whole orb on every single tick.
const PROGRESS_STEPS: f32 = 128.0;

/// One rendered player, kept so an unrelated repaint costs nothing.
struct Cached {
    orb: PlayerOrb,
    peaks: Arc<Vec<f32>>,
    path: PathBuf,
    state: PlayerState,
    started: Instant,
    frame: Arc<RenderImage>,
}

thread_local! {
    static PLAYERS: RefCell<HashMap<String, Cached>> = RefCell::new(HashMap::new());
    static HOVERED: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}

fn hovering(id: &str) -> bool {
    HOVERED.with(|hovered| hovered.borrow().get(id).copied().unwrap_or(false))
}

/// The frame for this instant, rendered only when something visible changed.
///
/// The recording orb renders on its own thread because it never stops moving.
/// A player sits still for most of its life, so rendering on demand and
/// remembering the frame is both cheaper and simpler: an unrelated keystroke
/// repaints the composer and finds the same texture waiting.
fn frame_for(
    id: &str,
    path: &Path,
    peaks: &Arc<Vec<f32>>,
    colour: u32,
    state: PlayerState,
    cx: &mut App,
) -> Arc<RenderImage> {
    let (frame, retired) = PLAYERS.with(|players| {
        let mut players = players.borrow_mut();
        if let Some(cached) = players.get(id)
            && cached.path == path
            && Arc::ptr_eq(&cached.peaks, peaks)
            && cached.state.progress == state.progress
            && cached.state.playing == state.playing
            && cached.state.hovered == state.hovered
        {
            return (Arc::clone(&cached.frame), None);
        }
        let previous = players.remove(id);
        let (orb, started, retired) = match previous {
            Some(old) => {
                let orb = if old.path == path && Arc::ptr_eq(&old.peaks, peaks) {
                    old.orb
                } else {
                    PlayerOrb::new(colour, peaks)
                };
                (orb, old.started, Some(old.frame))
            }
            None => (PlayerOrb::new(colour, peaks), Instant::now(), None),
        };
        let clock = started.elapsed().as_secs_f32();
        let frame = Arc::new(orb.render(PlayerState { clock, ..state }));
        players.insert(
            id.to_string(),
            Cached {
                orb,
                peaks: Arc::clone(peaks),
                path: path.to_path_buf(),
                state,
                started,
                frame: Arc::clone(&frame),
            },
        );
        (frame, retired)
    });
    if let Some(retired) = retired {
        cx.drop_image(retired, None);
    }
    frame
}

/// How far a press has to travel before it counts as a scrub rather than a tap.
const TAP_SLOP: f32 = 3.0;

/// Where a player sits on screen, recorded by the probe it paints.
type Track = Rc<RefCell<Option<[f32; 4]>>>;

/// One press on the glass, so a scrub and a tap can be told apart on release.
struct Press {
    id: String,
    from: f32,
    moved: bool,
}

thread_local! {
    static BOUNDS: RefCell<HashMap<String, Track>> = RefCell::new(HashMap::new());
    static PRESS: RefCell<Option<Press>> = const { RefCell::new(None) };
}

fn bounds_track(id: &str) -> Track {
    BOUNDS.with(|tracks| {
        Rc::clone(
            tracks
                .borrow_mut()
                .entry(id.to_string())
                .or_insert_with(|| Rc::new(RefCell::new(None))),
        )
    })
}

/// A transparent layer that records where the orb was painted.
///
/// Pointer events arrive in window coordinates, and the orb has to turn one of
/// them into a position along its band. That needs the rectangle it was drawn
/// in, and the probe is how a div learns its own.
fn bounds_probe(track: Track) -> impl IntoElement {
    canvas(
        |bounds, _window, _cx| bounds,
        move |bounds, _, _window, _cx| {
            let left = f32::from(bounds.origin.x);
            let top = f32::from(bounds.origin.y);
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            track
                .borrow_mut()
                .replace([left, top, left + width, top + height]);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full()
}

fn set_drag(id: &str, from: f32) {
    PRESS.with(|press| {
        *press.borrow_mut() = Some(Press {
            id: id.to_string(),
            from,
            moved: false,
        });
    });
}

/// Whether this press has become a scrub. Reports `true` once it has, so the
/// caller seeks on the move that crosses the threshold and on every one after.
fn drag_moved(id: &str, x: f32) -> bool {
    PRESS.with(|press| {
        let mut press = press.borrow_mut();
        let Some(held) = press.as_mut().filter(|held| held.id == id) else {
            return false;
        };
        if (x - held.from).abs() <= TAP_SLOP {
            return false;
        }
        held.moved = true;
        true
    })
}

/// End the press, reporting whether it was a tap rather than a scrub.
fn release_drag(id: &str) -> bool {
    PRESS.with(|press| {
        let mut press = press.borrow_mut();
        let tap = match press.as_ref() {
            Some(held) if held.id == id => !held.moved,
            _ => return false,
        };
        *press = None;
        tap
    })
}

fn fraction_at(id: &str, x: f32) -> Option<f32> {
    let track = BOUNDS.with(|tracks| tracks.borrow().get(id).cloned())?;
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
    if let Err(reason) = PlayerOrb::scrub(path, fraction) {
        logging::error(&format!("composer player: {reason}"));
    }
}

/// Where the playhead is and how long the take is.
///
/// The player knows both once a clip has been started, and the draft knows the
/// length before that, so the two are asked in that order. One helper, because
/// the band and the caption must never disagree about where the take ends.
fn span(live: Option<ClipState>, seconds: Option<f32>) -> (Duration, Option<Duration>) {
    let total = live
        .map(|state| state.duration)
        .filter(|duration| !duration.is_zero())
        .or_else(|| {
            seconds
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .map(Duration::from_secs_f32)
        });
    let position = live.map(|state| state.position).unwrap_or_default();
    (total.map_or(position, |total| position.min(total)), total)
}

/// The take as a player: the glass, its waveform, its band and its mark.
///
/// Nothing else belongs inside it. The discard control stays outside the
/// element so a click meant for the ✕ can never land on the glass, and the
/// transcript stays outside so the words are selectable text and not part of a
/// texture.
pub(crate) fn player(
    id: &str,
    path: &Path,
    seconds: Option<f32>,
    fallback_levels: &[f32],
    cx: &mut App,
) -> Div {
    let live = voice_play::state(path);
    let (position, total) = span(live, seconds);
    let fraction = total
        .filter(|total| !total.is_zero())
        .map(|total| (position.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let state = PlayerState {
        // Quantised so the band advances in half-point steps instead of
        // asking for a new texture on every tick of the dashboard.
        progress: (fraction * PROGRESS_STEPS).round() / PROGRESS_STEPS,
        playing: live.is_some_and(|state| state.playing),
        hovered: hovering(id),
        clock: 0.0,
    };
    let peaks = voice_media::visual(path)
        .map(|visual| Arc::clone(&visual.peaks))
        .unwrap_or_else(|| Arc::new(fallback_peaks(fallback_levels)));
    let frame = frame_for(id, path, &peaks, palette::current().amber, state, cx);

    let hover_id = id.to_string();
    let press_path = path.to_path_buf();
    let drag_path = path.to_path_buf();
    let down_id = id.to_string();
    let move_id = id.to_string();
    let up_id = id.to_string();
    let out_id = id.to_string();
    let painted = bounds_track(id);
    div()
        .id(SharedString::from(format!("voice-orb-{id}")))
        .relative()
        .flex_none()
        .size(px(ORB_POINTS))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                set_drag(&down_id, f32::from(event.position.x));
            },
        )
        .on_mouse_move(move |event: &MouseMoveEvent, window, cx| {
            if drag_moved(&move_id, f32::from(event.position.x)) {
                cx.stop_propagation();
                seek_at(&move_id, &drag_path, f32::from(event.position.x));
                window.refresh();
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            move |_event: &MouseUpEvent, window, _cx| {
                // A press that never travelled is a tap: it starts or stops the
                // take. A press that travelled was a scrub and has already
                // moved the playhead, so letting go must not also toggle.
                if release_drag(&up_id)
                    && let Err(reason) = PlayerOrb::activate(&press_path)
                {
                    logging::error(&format!("composer player: {reason}"));
                }
                window.refresh();
            },
        )
        .on_mouse_up_out(
            MouseButton::Left,
            move |_event: &MouseUpEvent, window, _cx| {
                release_drag(&out_id);
                window.refresh();
            },
        )
        .on_hover(move |over: &bool, window, _cx| {
            let changed = HOVERED.with(|hovered| {
                let mut hovered = hovered.borrow_mut();
                if hovered.get(&hover_id).copied().unwrap_or(false) == *over {
                    return false;
                }
                hovered.insert(hover_id.clone(), *over);
                true
            });
            if changed {
                window.refresh();
            }
        })
        .child(img(frame).absolute().inset_0().size_full())
        .child(bounds_probe(painted))
}

/// The take's position and length, for the caption beside the orb.
pub(crate) fn label(path: &Path, seconds: Option<f32>) -> String {
    let (position, total) = span(voice_play::state(path), seconds);
    let length = total
        .map(|total| clip_length(total.as_secs()))
        .unwrap_or_else(|| "--:--".to_string());
    format!("{} / {length}", clip_length(position.as_secs()))
}

/// Pool whatever the decoder produced down to the meter's own width, keeping
/// each bucket's peak so a quiet passage between two loud ones survives.
fn resample(peaks: &[f32]) -> [f32; VOICE_SAMPLES] {
    let mut levels = [0.0; VOICE_SAMPLES];
    if peaks.is_empty() {
        return levels;
    }
    let last = peaks.len() - 1;
    for (bucket, slot) in levels.iter_mut().enumerate() {
        let from = (bucket * peaks.len() / VOICE_SAMPLES).min(last);
        let to = ((bucket + 1) * peaks.len() / VOICE_SAMPLES)
            .max(from + 1)
            .min(peaks.len());
        let peak = peaks[from..to].iter().copied().fold(0.0_f32, f32::max);
        *slot = peak.clamp(0.0, 1.0);
    }
    levels
}

/// Coverage of a play triangle pointing right, anti-aliased by `feather`.
fn play_mark(nx: f32, ny: f32, feather: f32) -> f32 {
    // Signed distances to the two edges that are not the base: the vertical
    // one at the back, and the sloping pair that meet at the apex. Positive is
    // inside, so the smaller of the two is the distance to the nearest edge.
    let back = nx - GLYPH_BASE;
    let span = GLYPH_HALF_HEIGHT * (GLYPH_APEX - nx) / (GLYPH_APEX - GLYPH_BASE);
    let side = span - ny.abs();
    smooth(-feather, feather, back.min(side))
}

/// Coverage of two pause bars, anti-aliased by `feather`.
fn pause_mark(nx: f32, ny: f32, feather: f32) -> f32 {
    let vertical = (GLYPH_HALF_HEIGHT - ny.abs())
        .min((nx.abs() - GLYPH_BAR_INNER).min(GLYPH_BAR_OUTER - nx.abs()));
    smooth(-feather, feather, vertical)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read one pixel by its position on the sphere, in units of the orb
    /// radius, and return it as red, green, blue and alpha.
    fn sample(bytes: &[u8], size: u32, nx: f32, ny: f32) -> (f32, f32, f32, u8) {
        let scale = size as f32;
        let column = (((nx * ORB_RADIUS + 1.0) * 0.5 * scale) as usize).min(size as usize - 1);
        let row = (((ny * ORB_RADIUS + 1.0) * 0.5 * scale) as usize).min(size as usize - 1);
        let index = (row * size as usize + column) * 4;
        (
            bytes[index + 2] as f32,
            bytes[index + 1] as f32,
            bytes[index] as f32,
            bytes[index + 3],
        )
    }

    fn luma(colour: (f32, f32, f32, u8)) -> f32 {
        0.2126 * colour.0 + 0.7152 * colour.1 + 0.0722 * colour.2
    }

    /// One frame of a take whose peaks are all `peak`, so the silhouette shift
    /// is the same at every column and an assertion can name one radius.
    fn frame_at(size: u32, peak: f32, state: PlayerState) -> Vec<u8> {
        let player = PlayerOrb::at(size, 0xffbf47, &[peak; 112]);
        let image = player.render(state);
        image.as_bytes(0).expect("one frame").to_vec()
    }

    #[test]
    fn the_page_stays_readable_through_the_glass() {
        let size = 96;
        let bytes = frame_at(size, 0.0, PlayerState::default());
        assert_eq!(bytes[3], 0, "the corner is clear, not a black square");
        // A body point away from the band, the rim and the mark. The player has
        // to be thin enough to read a message through, and thick enough to read
        // as an object.
        let body = sample(&bytes, size, 0.0, -0.6).3 as f32 / 255.0;
        assert!(body > 0.25, "the glass is still an object, alpha {body}");
        assert!(
            body < 0.55,
            "the glass is too solid to read through, alpha {body}"
        );
    }

    #[test]
    fn the_silhouette_moves_with_the_take() {
        let size = 96;
        let quiet = frame_at(size, 0.0, PlayerState::default());
        let loud = frame_at(size, 1.0, PlayerState::default());
        // Just outside the resting silhouette. Silence leaves it clear; a full
        // take pushes the edge out far enough to paint there.
        assert_eq!(sample(&quiet, size, 0.0, 1.015).3, 0, "silence is a circle");
        assert!(
            sample(&loud, size, 0.0, 1.015).3 > 0,
            "the loudest take never reached the edge"
        );
    }

    #[test]
    fn the_band_fills_as_the_playhead_advances() {
        let size = 96;
        let early = PlayerState {
            progress: 0.3,
            playing: true,
            ..PlayerState::default()
        };
        let late = PlayerState {
            progress: 0.95,
            playing: true,
            ..PlayerState::default()
        };
        let before = frame_at(size, 0.0, early);
        let after = frame_at(size, 0.0, late);
        // The right end of the track is ahead of the playhead at 0.3 and behind
        // it at 0.95, so it has to brighten.
        let dim = luma(sample(&before, size, 0.7, 0.0));
        let lit = luma(sample(&after, size, 0.7, 0.0));
        assert!(lit > dim + 15.0, "the track did not fill: {dim} then {lit}");
        // The left end is behind the playhead in both frames and must not move.
        let steady_before = luma(sample(&before, size, -0.7, 0.0));
        let steady_after = luma(sample(&after, size, -0.7, 0.0));
        assert!(
            (steady_after - steady_before).abs() < 3.0,
            "the played part changed: {steady_before} then {steady_after}"
        );
    }

    #[test]
    fn the_mark_is_solid_and_follows_playback() {
        let size = 96;
        let paused = frame_at(size, 0.0, PlayerState::default());
        let playing = frame_at(
            size,
            0.0,
            PlayerState {
                playing: true,
                ..PlayerState::default()
            },
        );
        // A point inside the play triangle and inside the gap between the two
        // pause bars: covered while paused, clear while playing.
        let held = sample(&paused, size, 0.02, 0.0).3;
        let released = sample(&playing, size, 0.02, 0.0).3;
        assert!(held > 200, "the play mark is not solid, alpha {held}");
        assert!(
            held > released + 50,
            "the mark did not change with playback: {held} then {released}"
        );
    }

    #[test]
    fn the_band_is_the_brightest_event_in_the_body() {
        let size = 96;
        let bytes = frame_at(size, 0.0, PlayerState::default());
        let band = luma(sample(&bytes, size, 0.45, 0.0));
        let body = luma(sample(&bytes, size, 0.45, -0.6));
        assert!(
            band > body + 60.0,
            "the band stopped being the light: band {band}, body {body}"
        );
    }

    #[test]
    fn resample_pools_the_decoder_peaks() {
        // Two samples per bucket, one quiet and one loud: the loud one wins, so
        // a short peak between quiet neighbours survives the pooling.
        let mut peaks = Vec::new();
        for _ in 0..VOICE_SAMPLES {
            peaks.push(0.2);
            peaks.push(0.8);
        }
        let levels = resample(&peaks);
        assert!(
            levels.iter().all(|level| (level - 0.8).abs() < 1e-6),
            "the peak of each bucket is not what survived"
        );
        assert_eq!(resample(&[]), [0.0; VOICE_SAMPLES]);
        assert_eq!(resample(&[0.5]), [0.5; VOICE_SAMPLES]);
    }
}
