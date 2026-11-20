//! The composer's voice player, poured from the Seeker orb's material (#3092).
//!
//! A finished take is an object, not a row of controls. The glass holds it: the
//! take's own peaks lift the liquid sitting inside the block, the surface of
//! that liquid is the band of light the material has always carried across its
//! seam, and the whole block is the play button.
//!
//! The shape is a block, not a marble. The first cut of this module reused the
//! recording orb's sphere, which drew a circle in a row where every other
//! element is a rectangle: the material fitted, the silhouette did not. What
//! was fitted is the material (the heat ramp, the saturation curve, the warm
//! bowl under a cool dome, the dispersion, the dither) and none of that cares
//! what shape it is poured into. So the geometry here is a rounded box, the
//! fitted constants still come from `seeker_orb`, and only the things that are
//! genuinely per-surface live here: the bevel, the rim widths in pixels, and
//! where the liquid rests.
//!
//! Four deliberate differences from the recording orb:
//!
//! - the player has no worker thread. `OrbAnimation` renders every 33 ms until
//!   it is dropped, which is right for a microphone and wrong for a player that
//!   sits paused for most of its life. The player renders on demand and keeps
//!   its frame;
//! - the liquid is bottom-anchored, so a take reads as something poured into
//!   the block rather than a waveform hung in the middle of it;
//! - the body is thinner than the recording orb's, because a player is
//!   something you read a message through, not something you look at alone;
//! - the mark sits in its own lane at the left end, where the round play button
//!   used to be, so the liquid never has to share the middle with it.
//!
//! The recording strip draws the same block with the live microphone levels in
//! place of a take's peaks: one material, one shape, before and after the take
//! ends.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, RenderImage, SharedString,
    Styled, canvas, div, img, prelude::*, px,
};
use image::{Frame, RgbaImage};

use super::seeker_orb::{
    AMBER_HUE, AMBIENT, BAND_GAIN, BAND_REACH, BAND_SIGMA, BAND_WALL, BAYER, BOWL_ALPHA, BOWL_HUE,
    BREATHE, COOL_HUE, DISPERSION, DOME_ALPHA, EDGE_CAUSTIC, EDGE_COOL, EDGE_DEPTH, EDGE_SPREAD,
    EDGE_WARM, GLOW_GAIN, GLOW_SIGMA, MAX_LIFT, RIM_BASE, RIM_FALL_GAIN, RIM_FALL_SIGMA, RIM_GAIN,
    RIM_SILVER, RIM_TILT_X, RIM_TILT_Y, RIM_TOP_EDGE, RIM_TOP_FULL, RIM_TOP_GAIN, SCRIM_LUMA,
    SIDE_BIAS, VOICE_SAMPLES, WALL_ALPHA, WALL_EDGE, WALL_FULL, WARM_FOLLOW, bgra, gaussian,
    hash_noise, microphone_history, relative_luma, rgb_hsv, saturation_at, sine, smooth,
    smooth_voice, tinted, voice_at,
};
use super::voice_play::{self, ClipState};
use super::voice_preview::{clip_length, fallback_peaks};
use crate::{logging, palette, voice_media};

/// The corner radius of the block, in units of half its height.
///
/// Just under half the half-height: the corners are visibly rounded and the
/// sides stay straight for most of the block, which is what keeps it reading as
/// a block rather than a pill.
const SLAB_BEVEL: f32 = 0.42;

/// How wide the edge is allowed to fade, in pixels. A little over one is the
/// smallest that reads as smooth at the raster multiples used here.
const FEATHER_PX: f32 = 1.2;

/// The body is this much thinner than the recording orb's, so a message reads
/// through the glass while the take is held.
const BODY_TRANSMISSION: f32 = 0.62;

// The liquid ---------------------------------------------------------------

/// Where the liquid's floor sits, in units of half the height.
const LIQUID_FLOOR: f32 = 0.84;

/// How far a full-amplitude column rises off that floor.
///
/// The floor is at 0.84 and the rise is 1.34, so the loudest column reaches
/// -0.50: half the block is liquid at full scale and the top half stays clear
/// glass, which is where the cool dome and the rim live.
const LIQUID_RISE: f32 = 1.34;

/// How much of the width the liquid leaves to the wall at either end.
const LIQUID_PAD: f32 = 0.10;

/// The liquid's brightness at its own floor, and the extra it gains at the
/// surface. Light comes from above, so the surface is the lit part.
const LIQUID_BASE: f32 = 0.10;
const LIQUID_LIFT: f32 = 0.34;

/// How quickly the liquid dims with depth below its own surface.
const LIQUID_FALL: f32 = 2.6;

/// The swell that travels along the surface while the take is moving.
const SLOSH: f32 = 0.055;
const SLOSH_SPEED: f32 = 2.10;
const SLOSH_WAVES: f32 = 1.60;

// The track ----------------------------------------------------------------

/// How bright the part of the take that has not been played is, against the
/// part that has. It is a rail, not a hole: a person has to see the shape of
/// what is still coming.
const TRACK_AHEAD: f32 = 0.18;

/// The brightness when nothing is playing: a calm, even take.
const TRACK_REST: f32 = 0.42;

/// How soft the playhead's edge is along the track, in track units.
const BAND_SOFT: f32 = 0.012;

/// The playhead's own catch, where the light meets the part already played.
const PLAYHEAD_GAIN: f32 = 0.30;
const PLAYHEAD_WIDTH: f32 = 0.050;

// The rim ------------------------------------------------------------------

/// Where the rim ring sits, as a share of the bevel, and how wide it is.
///
/// These are pixel-scale rather than material-scale, which is why they are not
/// imported: the orb's own widths are fitted against a 320-pixel sphere, and a
/// block thirty points tall needs its own.
const RIM_BAND: f32 = 0.16;
const RIM_BAND_SIGMA: f32 = 0.10;

/// The overhead catch, the one edge of a block that always catches light.
const RIM_TOP_BAND: f32 = 0.20;
const RIM_TOP_SIGMA: f32 = 0.085;

// The mark ----------------------------------------------------------------

/// How far in from the left edge the mark's centre sits, in units of half the
/// height.
const MARK_INSET: f32 = 0.92;

/// The play and pause marks, in units of half the height from their centre.
const GLYPH_HALF_HEIGHT: f32 = 0.26;
const GLYPH_APEX: f32 = 0.24;
const GLYPH_BASE: f32 = -0.15;
const GLYPH_BAR_INNER: f32 = 0.035;
const GLYPH_BAR_OUTER: f32 = 0.155;
const GLYPH_STOP: f32 = 0.185;
const GLYPH_GAIN: f32 = 0.72;

/// How much a hovered block leans on its rim, so the target answers the pointer
/// before it is clicked.
const HOVER_RIM: f32 = 0.35;

/// The player's on-screen size in points, and the multiple it is rasterised at.
///
/// The frame is cached, so the cost is paid once per playhead step rather than
/// once per repaint.
const PLAYER_POINTS: (f32, f32) = (138.0, 30.0);
const PLAYER_SCALE: f32 = 2.5;

/// The recording strip's on-screen size in points, and its raster multiple.
///
/// Two rather than three: this one is redrawn thirty times a second, so it is
/// the only frame in the module whose cost is continuous.
pub(crate) const RECORDER_WIDTH: f32 = 78.0;
pub(crate) const RECORDER_HEIGHT: f32 = 30.0;
const RECORDER_SCALE: f32 = 2.0;

/// How many steps of the track the playhead takes before the frame is redrawn.
///
/// The block is under a hundred and forty points wide, so one step in a hundred
/// and twenty-eight is a point: invisible, and it keeps a repaint every forty
/// milliseconds from re-rasterising the glass on every single tick.
const PROGRESS_STEPS: f32 = 128.0;

/// The raster the block is drawn into, for an on-screen size and a multiple.
fn pixels(points: (f32, f32), scale: f32) -> (u32, u32) {
    (
        (points.0 * scale).round() as u32,
        (points.1 * scale).round() as u32,
    )
}

/// A block of the material: a rounded box distance field, one sample per raster
/// pixel.
///
/// Coordinates are in units of half the height. `y` runs from -1 at the top
/// edge to +1 at the bottom, the same way the recording orb's does, and `x`
/// runs from `-aspect` to `aspect`, so a unit is the same number of pixels in
/// both directions and the bevel stays circular on any block.
struct Slab {
    width: u32,
    height: u32,
    /// Half the width in units of half the height.
    aspect: f32,
    x: Vec<f32>,
    y: Vec<f32>,
    /// How far inside the edge this pixel sits, in units of half the height.
    /// Nought on the edge, and it reaches one at the vertical midline.
    inside: Vec<f32>,
    /// The surface normal's forward component: nought where the edge rolls
    /// away, one on the flat face.
    normal_z: Vec<f32>,
}

impl Slab {
    fn new(width: u32, height: u32) -> Self {
        let count = (width * height) as usize;
        let aspect = width as f32 / height as f32;
        let mut slab = Self {
            width,
            height,
            aspect,
            x: vec![0.0; count],
            y: vec![0.0; count],
            inside: vec![0.0; count],
            normal_z: vec![0.0; count],
        };
        let half_width = (aspect - SLAB_BEVEL).max(0.0);
        let half_height = 1.0 - SLAB_BEVEL;
        for row in 0..height {
            let y = ((row as f32 + 0.5) / height as f32) * 2.0 - 1.0;
            for column in 0..width {
                let across = ((column as f32 + 0.5) / width as f32) * 2.0 - 1.0;
                let x = across * aspect;
                let index = row as usize * width as usize + column as usize;
                slab.x[index] = x;
                slab.y[index] = y;
                // The rounded-box distance field. The straight parts of the
                // edge are the clamped offsets; the corners are the only place
                // the two axes have to be combined.
                let qx = x.abs() - half_width;
                let qy = y.abs() - half_height;
                let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
                let distance = outside + qx.max(qy).min(0.0) - SLAB_BEVEL;
                let inside = -distance;
                slab.inside[index] = inside;
                let turn = (1.0 - (inside / SLAB_BEVEL).clamp(0.0, 1.0)).clamp(0.0, 1.0);
                slab.normal_z[index] = (1.0 - turn * turn).max(0.0).sqrt();
            }
        }
        slab
    }
}

/// Which mark the block carries.
#[derive(Clone, Copy)]
enum Mark {
    Play,
    Pause,
    Stop,
}

/// One instant of the player, supplied by the composer each repaint.
#[derive(Clone, Copy, Default)]
pub(crate) struct PlayerState {
    /// Where the playhead sits, nought to one. Ignored while paused.
    pub(crate) progress: f32,
    /// Whether the clip is sounding. Decides the mark and the track.
    pub(crate) playing: bool,
    /// Whether the microphone is open on this block instead of a take.
    pub(crate) recording: bool,
    /// Whether the pointer is over the block.
    pub(crate) hovered: bool,
    /// Seconds since the block appeared, for the idle breath and the swell.
    pub(crate) clock: f32,
}

impl PlayerState {
    fn mark(&self) -> Mark {
        if self.recording {
            Mark::Stop
        } else if self.playing {
            Mark::Pause
        } else {
            Mark::Play
        }
    }

    /// Whether anything in the material is allowed to move. A paused player is
    /// a photograph of the take: drifting it would say it is doing something.
    fn moving(&self) -> bool {
        self.playing || self.recording
    }
}

/// A finished take drawn as the block. Owns the geometry, which is the
/// expensive part, and renders one frame per call.
pub(crate) struct PlayerBlock {
    slab: Slab,
    levels: [f32; VOICE_SAMPLES],
    colour: u32,
}

impl PlayerBlock {
    /// Build a player for one take. `peaks` is whatever the decoder produced,
    /// at whatever length, and is pooled down to the meter's own width.
    pub(crate) fn new(colour: u32, peaks: &[f32]) -> Self {
        Self::at(pixels(PLAYER_POINTS, PLAYER_SCALE), colour, peaks)
    }

    /// The same player at an arbitrary raster size, so the tests can render a
    /// small one instead of paying for a full frame per assertion.
    fn at(size: (u32, u32), colour: u32, peaks: &[f32]) -> Self {
        Self {
            slab: Slab::new(size.0, size.1),
            levels: smooth_voice(resample(peaks)),
            colour,
        }
    }

    /// Play or pause the take this block stands for.
    ///
    /// The audio lives in `voice_play`, which owns the output stream, the seek
    /// and the position. The composer asks the block, the block asks the
    /// player, so nothing outside this module has to know how a clip starts.
    pub(crate) fn activate(path: &Path) -> Result<(), String> {
        voice_play::toggle(path)
    }

    /// Move the playhead to `fraction` of the take, clamped to the track.
    pub(crate) fn scrub(path: &Path, fraction: f32) -> Result<(), String> {
        voice_play::seek(path, fraction.clamp(0.0, 1.0))
    }

    /// Render one frame. Straight alpha BGRA, the layout GPUI's atlas expects.
    pub(crate) fn render(&self, state: PlayerState) -> RenderImage {
        paint(&self.slab, &self.levels, self.colour, state)
    }
}

/// The block while a microphone is open (#3092).
///
/// The recording orb runs a worker because it never stops moving and its raster
/// is a full three-hundred-pixel sphere. This one is a fraction of that and is
/// redrawn from the composer's own thirty-three millisecond tick, so a thread
/// would buy nothing and cost a mutex, a shutdown flag and a texture the
/// composer has to remember to retire. It is drawn where it is needed instead.
pub(crate) struct LiveSlab {
    slab: Slab,
    levels: [f32; VOICE_SAMPLES],
    colour: u32,
    started: Instant,
    frame: Arc<RenderImage>,
}

impl LiveSlab {
    pub(crate) fn start(colour: u32) -> Self {
        let (width, height) = pixels((RECORDER_WIDTH, RECORDER_HEIGHT), RECORDER_SCALE);
        let slab = Slab::new(width, height);
        let levels = [0.0; VOICE_SAMPLES];
        let state = PlayerState {
            progress: 1.0,
            playing: true,
            recording: true,
            hovered: false,
            clock: 0.0,
        };
        let frame = Arc::new(paint(&slab, &levels, colour, state));
        Self {
            slab,
            levels,
            colour,
            started: Instant::now(),
            frame,
        }
    }

    /// Take this tick's microphone levels and redraw.
    ///
    /// `recording` is false once the microphone has been asked to stop: the WAV
    /// is still being built, so the block stays up, but the liquid settles
    /// instead of answering a device that is already closed.
    pub(crate) fn update(&mut self, recording: bool, levels: &[f32], colour: u32, cx: &mut App) {
        self.levels = microphone_history(levels);
        self.colour = colour;
        let state = PlayerState {
            progress: 1.0,
            playing: recording,
            recording,
            hovered: false,
            clock: self.started.elapsed().as_secs_f32(),
        };
        // Painted from the field rather than the argument, so the block always
        // redraws in the colour it is currently wearing.
        let next = Arc::new(paint(&self.slab, &self.levels, self.colour, state));
        let previous = std::mem::replace(&mut self.frame, next);
        cx.drop_image(previous, None);
    }

    pub(crate) fn frame(&self) -> Arc<RenderImage> {
        Arc::clone(&self.frame)
    }

    /// Retire the last texture from the atlas. Taking `self` is deliberate:
    /// there is nothing left to paint once this returns.
    pub(crate) fn dispose(self, cx: &mut App) {
        cx.drop_image(self.frame, None);
    }
}

/// The warm hue this accent asks for, taken the short way round the wheel.
///
/// `WARM_FOLLOW` is fitted to pull the bowl a quarter of the way from the
/// reference amber towards whatever colour the surface is wearing. Subtracting
/// the two hues directly breaks wherever hue wraps: red sits at 0.995 and the
/// reference amber at 0.109, so the plain difference reads as -0.886 and a
/// quarter of it lands at 0.276, in the greens. That is a red microphone
/// painting a green block, which is exactly what the recording strip did before
/// this went the short way: the same red is 0.114 the other way round, and the
/// bowl stays warm.
fn warm_hue_for(accent_hue: f32) -> f32 {
    let mut turn = accent_hue - AMBER_HUE;
    if turn > 0.5 {
        turn -= 1.0;
    } else if turn < -0.5 {
        turn += 1.0;
    }
    BOWL_HUE + turn * WARM_FOLLOW
}

/// Draw one frame of the block.
fn paint(
    slab: &Slab,
    levels: &[f32; VOICE_SAMPLES],
    colour: u32,
    state: PlayerState,
) -> RenderImage {
    let width = slab.width;
    let height = slab.height;
    let aspect = slab.aspect;
    let mut pixels = RgbaImage::new(width, height);
    let accent_hue = rgb_hsv(colour).0;
    let moving = state.moving();
    let breathe = if moving {
        BREATHE * (sine(state.clock * 0.70) - 0.5)
    } else {
        0.0
    };
    let hover = if state.hovered { HOVER_RIM } else { 0.0 };
    let feather = FEATHER_PX / (height as f32 * 0.5);
    let mark = state.mark();
    let mark_x = -aspect + MARK_INSET;
    let travel = (aspect - LIQUID_PAD).max(0.01);
    let head_x = -aspect + LIQUID_PAD + state.progress.clamp(0.0, 1.0) * 2.0 * travel;

    // Everything that depends on the column and not the row is worked out once
    // per column instead of once per pixel: the liquid's height, how lit this
    // stretch of track is, and how far the band reaches across the block.
    let mut surfaces = vec![0.0_f32; width as usize];
    let mut tracks = vec![0.0_f32; width as usize];
    let mut laterals = vec![0.0_f32; width as usize];
    for column in 0..width as usize {
        let x = slab.x[column];
        let along = ((x + aspect - LIQUID_PAD) / (2.0 * travel)).clamp(0.0, 1.0);
        let level = voice_at(levels, along);
        let swell = if moving {
            SLOSH * sine(state.clock * SLOSH_SPEED + along * SLOSH_WAVES * std::f32::consts::TAU)
        } else {
            0.0
        };
        surfaces[column] = LIQUID_FLOOR - level * LIQUID_RISE + swell + breathe;
        let played = smooth(-BAND_SOFT, BAND_SOFT, state.progress - along);
        tracks[column] = if moving {
            TRACK_AHEAD + (1.0 - TRACK_AHEAD) * played
        } else {
            TRACK_REST
        };
        laterals[column] =
            BAND_WALL + (1.0 - BAND_WALL) * gaussian((along - 0.5) * 2.0, BAND_REACH);
    }

    for (index, pixel) in pixels.pixels_mut().enumerate() {
        let inside = slab.inside[index];
        let edge = smooth(0.0, feather, inside);
        if edge <= 0.0 {
            continue;
        }
        let column = index as u32 % width;
        let x = slab.x[index];
        let y = slab.y[index];
        let nz = slab.normal_z[index];
        let depth = (inside / SLAB_BEVEL).clamp(0.0, 1.0);
        let across = x / aspect;
        let surface = surfaces[column as usize];
        let track = tracks[column as usize];
        let lateral = laterals[column as usize];

        // The liquid: everything below its own surface. That surface is the
        // seam the material has always had, so the band of light that used to
        // ride the equator now rides the top of the take.
        let submerge = smooth(surface - feather, surface + feather, y);
        let dome = 1.0 - submerge;
        let meniscus = gaussian(y - surface, BAND_SIGMA);
        let below = (y - surface).max(0.0);
        let liquid = submerge * (LIQUID_BASE + LIQUID_LIFT * (-below * LIQUID_FALL).exp());
        let glow = gaussian(y - surface, GLOW_SIGMA) * (1.0 + SIDE_BIAS * -across) * lateral;

        // The mark: a play triangle while paused, two bars while playing, a
        // stop square while the microphone is open. It sits at the left end and
        // punches through the light, so the liquid frames it instead of washing
        // it out.
        let glyph = match mark {
            Mark::Play => play_mark(x - mark_x, y, feather),
            Mark::Pause => pause_mark(x - mark_x, y, feather),
            Mark::Stop => stop_mark(x - mark_x, y, feather),
        };

        // The wall caustic: the narrow chromatic band just inside the rim, cool
        // above the seam and warm below.
        let edge_band = gaussian(inside - (1.0 - EDGE_DEPTH), EDGE_SPREAD) * across * across;
        let edge_gain = EDGE_WARM + (EDGE_COOL - EDGE_WARM) * dome;
        let caustic = edge_band * edge_gain;

        // The border, placed by distance from the edge so the ring follows the
        // bevel on every side rather than a fixed circle.
        let rim_key = (0.5 - 0.5 * (across * RIM_TILT_X + y * RIM_TILT_Y)).max(0.0);
        let rim_ring = gaussian(depth - RIM_BAND, RIM_BAND_SIGMA);
        let rim_arc = smooth(RIM_TOP_EDGE, RIM_TOP_FULL, y);
        let rim_top = gaussian(depth - RIM_TOP_BAND, RIM_TOP_SIGMA) * rim_arc;

        // The playhead: a line where the played part meets the rest.
        let head = if state.playing && !state.recording {
            PLAYHEAD_GAIN * gaussian(x - head_x, PLAYHEAD_WIDTH) * smooth(0.0, 0.25, depth)
        } else {
            0.0
        };

        // The mark is a hole in the light, not a shape painted on top of it:
        // the band and its halo step aside so the white glyph is not swallowed
        // by the brightest thing in the material.
        let lit = 1.0 - glyph;
        let heat = (AMBIENT
            + BAND_GAIN * meniscus * lateral * track * lit
            + GLOW_GAIN * glow * track * lit
            + liquid * track
            + RIM_FALL_GAIN * (-inside / RIM_FALL_SIGMA).exp()
            + RIM_GAIN * rim_ring * (RIM_BASE + (1.0 - RIM_BASE) * rim_key) * (1.0 + hover)
            + RIM_TOP_GAIN * rim_top
            + caustic
            + head
            + GLYPH_GAIN * glyph)
            .clamp(0.0, 1.0);

        let base_saturation = saturation_at(heat);
        let saturation = base_saturation + (EDGE_CAUSTIC - base_saturation) * edge_band.min(1.0);
        // The material splits its hues through the thickest part of the glass,
        // so the block gets the same fringe: warm in the liquid, cool above it.
        let fringe = DISPERSION * inside.clamp(0.0, 1.0).powi(6);
        let warm_hue = warm_hue_for(accent_hue);
        let warm = tinted(warm_hue - fringe, saturation, heat);
        let cool = tinted(COOL_HUE + fringe, saturation, heat);
        let mut red = warm.0 * submerge + cool.0 * dome;
        let mut green = warm.1 * submerge + cool.1 * dome;
        let mut blue = warm.2 * submerge + cool.2 * dome;
        // The mark and the overhead catch lose their hue: a play button is
        // white, not a lit cyan shape.
        let silver = (RIM_SILVER * rim_top + glyph).clamp(0.0, 1.0);
        red += (1.0 - red) * silver;
        green += (1.0 - green) * silver;
        blue += (1.0 - blue) * silver;

        // Density from geometry, thinner than the recording orb's. The mark is
        // the one part of the block that is solid, so it stays legible over
        // whatever the glass is sitting on.
        let deep = BOWL_ALPHA + (DOME_ALPHA - BOWL_ALPHA) * dome;
        let chord = 0.55 + 0.45 * nz;
        let wall = across * across * smooth(WALL_FULL, WALL_EDGE, 1.0 - depth);
        let density = ((deep * chord * (1.0 - wall) + WALL_ALPHA * wall) * BODY_TRANSMISSION)
            .max(glyph * 0.88)
            .clamp(0.0, 1.0);

        // The fitted values are the reference's composited pixels, so over the
        // dark scrim the emitted colour is lifted to land on them.
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
        let row = index as u32 / width;
        let dither = ((BAYER[((row & 3) * 4 + (column & 3)) as usize] - 0.5) * 1.4
            + (hash_noise(index) - 0.5) * 0.8)
            / 255.0;
        *pixel = bgra(red + dither, green + dither, blue + dither, alpha + dither);
    }
    RenderImage::new(vec![Frame::new(pixels)])
}

/// One rendered player, kept so an unrelated repaint costs nothing.
struct Cached {
    block: PlayerBlock,
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
            && cached.path.as_path() == path
            && Arc::ptr_eq(&cached.peaks, peaks)
            && cached.state.progress == state.progress
            && cached.state.playing == state.playing
            && cached.state.hovered == state.hovered
        {
            return (Arc::clone(&cached.frame), None);
        }
        let previous = players.remove(id);
        let (block, started, retired) = match previous {
            Some(old) => {
                let block = if old.path.as_path() == path && Arc::ptr_eq(&old.peaks, peaks) {
                    old.block
                } else {
                    PlayerBlock::new(colour, peaks)
                };
                (block, old.started, Some(old.frame))
            }
            None => (PlayerBlock::new(colour, peaks), Instant::now(), None),
        };
        let clock = started.elapsed().as_secs_f32();
        let frame = Arc::new(block.render(PlayerState { clock, ..state }));
        players.insert(
            id.to_string(),
            Cached {
                block,
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

/// A transparent layer that records where the block was painted.
///
/// Pointer events arrive in window coordinates, and the block has to turn one
/// of them into a position along its track. That needs the rectangle it was
/// drawn in, and the probe is how a div learns its own.
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
    if let Err(reason) = PlayerBlock::scrub(path, fraction) {
        logging::error(&format!("composer player: {reason}"));
    }
}

/// Where the playhead is and how long the take is.
///
/// The player knows both once a clip has been started, and the draft knows the
/// length before that, so the two are asked in that order. One helper, because
/// the track and the caption must never disagree about where the take ends.
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

/// The take as a player: the block, its liquid, its track and its mark.
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
) -> impl IntoElement {
    let live = voice_play::state(path);
    let (position, total) = span(live, seconds);
    let fraction = total
        .filter(|total| !total.is_zero())
        .map(|total| (position.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let state = PlayerState {
        // Quantised so the track advances in sub-point steps instead of asking
        // for a new texture on every tick of the dashboard.
        progress: (fraction * PROGRESS_STEPS).round() / PROGRESS_STEPS,
        playing: live.is_some_and(|state| state.playing),
        recording: false,
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
        .w(px(PLAYER_POINTS.0))
        .h(px(PLAYER_POINTS.1))
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
                    && let Err(reason) = PlayerBlock::activate(&press_path)
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

/// The take's position and length, for the caption beside the block.
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
    // Signed distances to the two edges that are not the base: the vertical one
    // at the back, and the sloping pair that meet at the apex. Positive is
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

/// Coverage of a stop square, anti-aliased by `feather`. The recording strip
/// ends the take, so its mark says stop rather than play.
fn stop_mark(nx: f32, ny: f32, feather: f32) -> f32 {
    let square = GLYPH_STOP - nx.abs().max(ny.abs());
    smooth(-feather, feather, square)
}

#[cfg(test)]
mod tests {
    use super::seeker_orb::{BOWL_HUE, rgb_hsv};
    use super::*;

    /// Read one pixel at a point on the block, in units of half the height, and
    /// return it as red, green, blue and alpha.
    fn sample(bytes: &[u8], slab: &Slab, x: f32, y: f32) -> (f32, f32, f32, u8) {
        let column = (((x / slab.aspect + 1.0) * 0.5 * slab.width as f32) as usize)
            .min(slab.width as usize - 1);
        let row = (((y + 1.0) * 0.5 * slab.height as f32) as usize).min(slab.height as usize - 1);
        let index = (row * slab.width as usize + column) * 4;
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

    /// One frame of a take whose peaks are all `peak`, so the liquid stands at
    /// the same height in every column and an assertion can name one row.
    fn frame_at(size: (u32, u32), peak: f32, state: PlayerState) -> (Slab, Vec<u8>) {
        let block = PlayerBlock::at(size, 0xffbf47, &[peak; 112]);
        let image = block.render(state);
        let slab = Slab::new(size.0, size.1);
        (slab, image.as_bytes(0).expect("one frame").to_vec())
    }

    fn small() -> (u32, u32) {
        (120, 32)
    }

    #[test]
    fn the_block_is_not_a_disc() {
        let (slab, bytes) = frame_at(small(), 0.0, PlayerState::default());
        // A disc of this height would have ended at x = 1. The block is still
        // glass out here, which is the whole point of the shape.
        let wide = sample(&bytes, &slab, slab.aspect * 0.85, 0.0).3;
        assert!(
            wide > 0,
            "the block stops short of its own width, alpha {wide}"
        );
        // And the corner it has instead of that disc's arc is clear.
        let corner = sample(&bytes, &slab, slab.aspect * 0.99, 0.97).3;
        assert_eq!(corner, 0, "the corner should be clear, alpha {corner}");
    }

    #[test]
    fn the_page_stays_readable_through_the_glass() {
        let (slab, bytes) = frame_at(small(), 0.0, PlayerState::default());
        // A point in the clear air above a silent take's liquid. The player has
        // to be thin enough to read a message through, and thick enough to read
        // as an object.
        let body = sample(&bytes, &slab, 0.0, -0.55).3 as f32 / 255.0;
        assert!(body > 0.25, "the glass is still an object, alpha {body}");
        assert!(
            body < 0.75,
            "the glass is too solid to read through, alpha {body}"
        );
    }

    #[test]
    fn the_liquid_rises_with_the_take() {
        let size = small();
        let (slab, quiet) = frame_at(size, 0.0, PlayerState::default());
        let (_, loud) = frame_at(size, 1.0, PlayerState::default());
        // A row that is clear air over a silent take and deep liquid under a
        // full one. The liquid is warmer and brighter than the glass above it.
        let dry = luma(sample(&quiet, &slab, 0.0, 0.20));
        let wet = luma(sample(&loud, &slab, 0.0, 0.20));
        assert!(
            wet > dry + 20.0,
            "the liquid did not rise with the take: {dry} then {wet}"
        );
    }

    #[test]
    fn the_track_fills_as_the_playhead_advances() {
        let size = small();
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
        let (slab, before) = frame_at(size, 0.4, early);
        let (_, after) = frame_at(size, 0.4, late);
        // The right end of the track is ahead of the playhead at 0.3 and behind
        // it at 0.95, so it has to brighten.
        let dim = luma(sample(&before, &slab, slab.aspect * 0.55, 0.55));
        let lit = luma(sample(&after, &slab, slab.aspect * 0.55, 0.55));
        assert!(lit > dim + 12.0, "the track did not fill: {dim} then {lit}");
        // The left end is behind the playhead in both frames and must not move.
        let steady_before = luma(sample(&before, &slab, -slab.aspect * 0.55, 0.55));
        let steady_after = luma(sample(&after, &slab, -slab.aspect * 0.55, 0.55));
        assert!(
            (steady_after - steady_before).abs() < 6.0,
            "the played part changed: {steady_before} then {steady_after}"
        );
    }

    #[test]
    fn the_mark_is_solid_and_follows_playback() {
        let size = small();
        let (slab, paused) = frame_at(size, 0.0, PlayerState::default());
        let (_, playing) = frame_at(
            size,
            0.0,
            PlayerState {
                playing: true,
                ..PlayerState::default()
            },
        );
        // A point inside the play triangle and inside the gap between the two
        // pause bars: covered while paused, clear while playing.
        let mark_x = -slab.aspect + MARK_INSET;
        let held = sample(&paused, &slab, mark_x + 0.02, 0.0).3;
        let released = sample(&playing, &slab, mark_x + 0.02, 0.0).3;
        assert!(held > 200, "the play mark is not solid, alpha {held}");
        assert!(
            held > released + 50,
            "the mark did not change with playback: {held} then {released}"
        );
    }

    #[test]
    fn the_recording_mark_is_a_stop_square() {
        let size = small();
        let state = PlayerState {
            recording: true,
            playing: true,
            progress: 1.0,
            ..PlayerState::default()
        };
        let (slab, bytes) = frame_at(size, 0.0, state);
        let mark_x = -slab.aspect + MARK_INSET;
        // The centre of a stop square is solid, and the same point is a hole in
        // the play triangle, whose base sits behind it.
        let centre = sample(&bytes, &slab, mark_x, 0.0).3;
        assert!(centre > 200, "the stop mark is not solid, alpha {centre}");
    }

    #[test]
    fn the_band_is_the_brightest_event_in_the_body() {
        let (slab, bytes) = frame_at(small(), 0.5, PlayerState::default());
        // The liquid stands at 0.84 - 0.67, so the band is the row at 0.17 and
        // the body is the row well below it.
        let band = luma(sample(&bytes, &slab, 0.0, 0.17));
        let body = luma(sample(&bytes, &slab, 0.0, 0.55));
        assert!(
            band > body + 40.0,
            "the band stopped being the light: band {band}, body {body}"
        );
    }

    #[test]
    fn a_red_microphone_paints_a_red_block() {
        // The recording strip wears the palette's red, and red sits on the far
        // side of the hue seam from the reference amber. Subtracting the two
        // hues directly put the bowl at 0.276, a green block under a red
        // microphone, so the turn has to go the short way round.
        let red = 0xe5484d;
        let hue = warm_hue_for(rgb_hsv(red).0);
        assert!(
            (hue - BOWL_HUE).abs() < 0.05,
            "a red accent moved the bowl to {hue}, away from the glass"
        );
        // And the pixels agree with the hue: deep in the liquid of a red take
        // the warm channel leads, which a green block could not do.
        let block = PlayerBlock::at(small(), red, &[0.9; 112]);
        let image = block.render(PlayerState {
            recording: true,
            playing: true,
            progress: 1.0,
            ..PlayerState::default()
        });
        let slab = Slab::new(small().0, small().1);
        let bytes = image.as_bytes(0).expect("one frame");
        let (red, green, _, alpha) = sample(bytes, &slab, 0.0, 0.62);
        assert!(alpha > 40, "there is no liquid to judge, alpha {alpha}");
        assert!(
            red > green,
            "the red microphone painted a block of green: {red} against {green}"
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
