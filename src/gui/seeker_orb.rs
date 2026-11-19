//! Procedural Siri-style glass for the Seeker's recording bubble (#3044).
//!
//! GPUI cannot sample and refract the window behind one element, so this draws
//! the material itself, per pixel, from a sphere's own geometry:
//!
//! - a real surface normal, so the body is lit from one direction instead of
//!   fading evenly in every direction;
//! - the five values a retoucher builds by hand, moved with dodge and burn
//!   rather than mixed towards white;
//! - a border with thickness: an exponential shoulder just inside the
//!   silhouette, and one thin specular ring sitting on it, keyed to the upper
//!   left where the reference's own ring is brightest;
//! - a horizon, cool above and warm below, with a thin band of light on the
//!   seam;
//! - a hard circular silhouette, so the image's own square boundary never
//!   paints a wash over the scrim.
//!
//! A worker produces the frames and the UI only uploads the newest one, keeping
//! the pixel math off the UI thread.

use gpui::{App, RenderImage};
use image::{Frame, Rgba, RgbaImage};
use std::cell::RefCell;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Sized to the bubble it is painted into, not to the screen.
///
/// The bubble is `TAKE_BUBBLE` rems wide and the rem is 16 px, so it covers
/// 144 points: 288 device pixels on a Retina display, and more only when
/// someone zooms past 2x. Everything above that was wasted work, because the
/// material is a smooth gradient with no fine detail for extra pixels to
/// carry, and the image is resampled into the box anyway. A 1024 px frame
/// cost 22.79 ms to build, and this one carries a tenth of the pixels.
pub(crate) const ORB_PIXELS: u32 = 320;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// How much of the half-frame the glass fills.
///
/// The margin is the anti-aliased edge's room to run in, nothing more. A
/// narrower orb looked identical and cost the Fresnel rim its last two pixels,
/// because the rim lives exactly where a tighter mask was fading out: the one
/// bright thing in the material was being multiplied away by the silhouette.
const ORB_RADIUS: f32 = 0.98;
const SINE_STEPS: usize = 4096;
const VOICE_SAMPLES: usize = super::voice_input::METER_BARS;

/// The light budget, and the one number that decides whether this reads as
/// glass or as paint.
///
/// `heat` is the pixel's brightness as a fraction of white, so the field below
/// is measured directly against the reference: point a probe at a latitude and
/// the value it returns is the luma that latitude should carry, nought to one.
/// Fitted to the reference's own pixels at thirteen latitudes, residual 0.016.
///
/// The shipped version instead mixed a lambert dodge into `HEAT_BASE` and
/// pushed the total through a ramp whose darkest stop was a saturated amber at
/// luma 54. Every pixel therefore carried gold paint and no part of the body
/// could reach the reference's `(16, 18, 20)`, so a dark marble rendered as a
/// solid lit sphere. Nothing about the falloff could fix that; the ramp had to
/// stop owning brightness.
const AMBIENT: f32 = 0.0672;

/// The band is the one hot thing in the material: a narrow spike on the seam.
/// Its width is what makes or breaks the figure. Wide enough and it becomes a
/// stripe painted across a disc; this width puts its half-maximum 0.13 of the
/// radius apart, which is a lens catching a backlight.
const BAND_GAIN: f32 = 0.3886;
const BAND_SIGMA: f32 = 0.0562;

/// How far the seam reaches sideways before it dies out.
///
/// A horizon with no lateral falloff runs to the silhouette at full brightness,
/// and a stripe that hits both edges at once is paint, not light: the eye reads
/// it as laid over a disc rather than coming from inside one. A lens stops
/// lighting the seam where the glass turns away, so this fades the band and the
/// halo out towards the wall.
const BAND_REACH: f32 = 0.55;

/// What the band and its halo still carry at the wall, as a fraction of the
/// centre.
///
/// A plain gaussian with `BAND_REACH` is already down to a fifth by the
/// silhouette, which is the dark gap at the ends of the glow: the light stopped
/// before the glass did. Holding a floor at the limb lets the glow meet the
/// inner wall, while the gaussian above it still gives the falloff its
/// spherical shape.
const BAND_WALL: f32 = 0.32;

/// How far the band's own seam rises towards the wall, as a fraction of the
/// radius.
///
/// The bowl's seam bows down (`BOW_BOWL`), which is right for where the warm
/// side starts, but the band itself is the horizon reflected off the inside of
/// a sphere, and that rises at the limb. Giving the strip its own arch is what
/// wraps it onto the glass instead of running it flat and stopping it short of
/// both walls.
const BAND_ARCH: f32 = 0.16;

/// The halo the band throws into both halves, which is what stops the seam
/// reading as a drawn line and lets the temperature bleed into the bowl.
const GLOW_GAIN: f32 = 0.585;
const GLOW_SIGMA: f32 = 0.1922;

/// A low lift under the horizon, so the lower half has depth behind the glass
/// instead of black showing through it. Small on purpose: the reference's bowl
/// falls to luma 35 and stays there.
const BOWL_LIFT: f32 = 0.0192;

/// How far the key pushes the halo towards the upper left.
///
/// A purely vertical field is a surface of revolution, and a surface of
/// revolution reads as a flat graphic however good its profile is. This is the
/// only asymmetry in the light, and it is deliberately small: measured against
/// the reference, the left of the band carries about 28 more luma than the
/// right, and nothing in the bowl, which is near black on both sides.
const SIDE_BIAS: f32 = 0.26;

/// The border the glass presents at its own silhouette, as three terms.
///
/// Measured off the reference's own edge, sampling radially from its centre at
/// (118, 107) with a radius of 95.5 px:
///
/// * outside the disc, 17 luma;
/// * one pixel of rim, 63 luma;
/// * the ring's peak, at r 0.995, 88 luma;
/// * two pixels inside it, 39 luma;
/// * five pixels inside it, 28 luma;
/// * the body's floor below r 0.85, 18 luma.
///
/// So the border is one thin bright ring sitting ON the silhouette, and
/// everything inside it falls away exponentially. There is no second bright
/// ring further in and no black trough: an earlier pass put a bevel at r 0.80
/// and a 0.34-deep trough at r 0.90, both invented, and that pair is exactly
/// what read as a soft double ring.
///
/// The ring is the only hard event in the material, which is why its width is a
/// hundredth of the radius: wide enough to still land on a pixel at
/// `ORB_PIXELS`, narrow enough to stay a specular edge rather than a drawn
/// outline. `RIM_TILT_*` aims it at the upper left, where the reference's ring
/// is bright (88 luma at the top, 64 at the left, 30 at the bottom).
const RIM_RADIUS: f32 = 0.995;
const RIM_WIDTH: f32 = 0.0058;
const RIM_GAIN: f32 = 0.26;
const RIM_BASE: f32 = 0.02;
const RIM_TILT_X: f32 = 0.55;
const RIM_TILT_Y: f32 = 0.83;

/// The overhead key's own catch on the top arc, ten o'clock to two.
///
/// The keyed ring above is one gradient running the whole silhouette, so the
/// brightest it can be overhead is the same gain that also has to stay dim
/// along the bottom. The reference does not work that way: its top arc is a
/// separate, much brighter event, a sharp silver-white band that exists only
/// overhead and falls off by ten and two. This term is that event, and it is
/// what makes the ring read as a reflection rather than as an outline.
///
/// Its width is narrower than the ring's so the catch stays a one-pixel
/// specular at `ORB_PIXELS` instead of blooming into the shoulder behind it.
const RIM_TOP_GAIN: f32 = 0.42;
const RIM_TOP_WIDTH: f32 = 0.0050;
const RIM_TOP_EDGE: f32 = -0.50;
const RIM_TOP_FULL: f32 = -0.90;

/// How far the overhead catch is pulled towards white.
///
/// A specular that bright has lost nearly all of its hue, and the reference's
/// top arc reads silver rather than cyan. The pull is applied after the tonal
/// colour and before the composite correction, so it moves hue only: the
/// correction still lands the pixel on the light field's own brightness.
const RIM_SILVER: f32 = 0.50;

/// The shoulder the glass shows as it turns away, just inside the ring. This is
/// what gives the edge thickness: without it the ring floats on the silhouette
/// with nothing behind it and the object reads as a sticker.
const RIM_FALL_GAIN: f32 = 0.11;
const RIM_FALL_SIGMA: f32 = 0.045;

/// The horizon: the seam between the cool dome and the warm bowl, plus the
/// thin band of light that sits on it.
///
/// The seam sits on the equator rather than above it, because the reference's
/// brightest pixel is at the centre to within a hundredth of the radius. The
/// bow is small and grows towards the wall through the same lens the ribbons
/// ride, so the marble has a top and a bottom instead of one radial fade. One
/// radial fade is exactly what reads as a lit plate.
const HORIZON: f32 = 0.0;
const BOW_BOWL: f32 = 0.10;

/// The seam breathes, and this is by how much of the radius.
///
/// The reference is a single still, so its band is at rest. The orb is not, and
/// a seam that never moves reads as a printed decal. A hundredth of the radius
/// is invisible as a shift on its own and is what carries the breathing.
const BREATHE: f32 = 0.010;

/// The two temperatures the seam splits the body into, as hues.
///
/// Only hue follows the theme now. Brightness is the light field's job and
/// saturation comes from the reference, because those two are what make the
/// material; letting the accent drive them is what turned every theme's orb
/// into a solid gold ball. The dome's cool is fixed: rotating a green accent by
/// whatever angle cools an amber one lands somewhere else entirely, so a
/// derived cool loses the split on half the themes.
const COOL_HUE: f32 = 0.586;

/// The reference's warm hue, and the accent hue the fit was measured at.
///
/// `WARM_FOLLOW` moves the warm side a quarter of the way from that reference
/// point towards the live accent, so a themed orb keeps a trace of its palette
/// without the bowl leaving the range a glass bowl occupies.
const BOWL_HUE: f32 = 0.055;
const AMBER_HUE: f32 = 0.109;
const WARM_FOLLOW: f32 = 0.25;

/// Saturation against brightness, which is the curve that keeps the middle warm
/// without the ends turning to paint.
///
/// A tonal ramp cannot carry this: the same amber that reads as glass at
/// mid-brightness reads as gold leaf at full brightness, so saturation has to
/// fall away as the value climbs, and it has to fall away again below the band
/// or the shadow side turns to mud. Measured off the reference at seven levels.
const HEAT_STOPS: [f32; 7] = [0.00, 0.15, 0.30, 0.50, 0.75, 0.93, 1.00];
const SAT_STOPS: [f32; 7] = [0.10, 0.15, 0.34, 0.50, 0.55, 0.18, 0.05];

/// How much of the backdrop the material passes.
///
/// This is what makes it glass rather than a painting of glass. The body is
/// smoke: it lets the page behind it through, and only the band and the ring go
/// dense. The floor is where the darkest part sits, because a body that went
/// fully opaque would hide the scrim and read as a solid again.
///
/// The floor is almost the whole story, too: at 0.34 it carried about eighty
/// percent of the body's opacity, with the entire light field accounting for
/// the rest. Lowering it to 0.15 takes the body from forty-two percent paint to
/// twenty-two, which is the difference between the page showing through and the
/// page being veiled by a grey film. The band does not move at all, because
/// `heat` saturates there and the glass is already at one.
///
/// 0.15 is the no-clip boundary, not a taste call: the worst lift the body asks
/// for is 3.33 against `MAX_LIFT` at 3.4. Below that the correction clips and
/// the body starts reading darker than the reference, which is why the two
/// constants are documented against each other.
const GLASS_FLOOR: f32 = 0.15;

/// What the glass is composited onto, as the luma of that resolved colour.
///
/// The Seeker scrim is `rgba(0x08080acc)` over the dark window's `0x101010`
/// background, which resolves to `(10, 10, 11)` and a luma of 0.0395. The
/// lifted emission below needs this number, so it is written down rather than
/// assumed: getting it wrong darkens or washes the whole marble by a visible
/// margin.
const SCRIM_LUMA: f32 = 0.0395;

/// The most the emission may be lifted before the correction gives up.
///
/// In the darkest smoke the lift is already large; letting it run free would
/// push the near-black body to a visible grey and destroy the shadow that gives
/// the object its depth.
const MAX_LIFT: f32 = 3.4;

/// Depth: the far wall, and the fringe at the silhouette.
///
/// `FAR_LIFT` is the band's image refracted through the body, so it sits low,
/// off the seam, and dim. It is the only cue in the material that says there is
/// a second surface behind the first, and without it the orb is a disc.
///
/// `DISPERSION` splits the rim sliver by a few hundredths of a turn of hue,
/// which is what glass does at a silhouette.
const FAR_WALL: f32 = 0.041;
const FAR_LIFT: f32 = 0.42;
const DISPERSION: f32 = 0.022;

/// The dome's softbox: a wide, shallow arc on the upper left.
///
/// Measured off the reference, this is not a glint. It is a soft-edged slab
/// that follows the spherical contour from about nine o'clock to twelve, so it
/// has to be placed by radius and by direction rather than by distance from one
/// point: a point-and-sigma catch is a round dot, and a round dot is exactly
/// what read as a painted highlight on the earlier passes.
///
/// The arc sits at 0.76 of the radius along a bearing of roughly 10:30, which
/// is where the reference's slab sits. `SPEC_WIDTH` is its radial thickness and
/// the span terms cut it off by angle, so the shape is a trapezoid bent to the
/// glass instead of a disc.
const SPEC_RADIUS: f32 = 0.76;
const SPEC_DIR_X: f32 = -0.579;
const SPEC_DIR_Y: f32 = -0.815;
const SPEC_WIDTH: f32 = 0.105;
const SPEC_SPAN_LOW: f32 = 0.62;
const SPEC_SPAN_HIGH: f32 = 0.97;
const SPEC_GAIN: f32 = 0.22;

/// A 4x4 ordered dither, already normalised so the loop only subtracts.
const BAYER: [f32; 16] = [
    0.0, 0.5, 0.125, 0.625, 0.75, 0.25, 0.875, 0.375, 0.1875, 0.6875, 0.0625, 0.5625, 0.9375,
    0.4375, 0.8125, 0.3125,
];

#[derive(Clone, Copy)]
struct OrbInput {
    recording: bool,
    /// Recent microphone peaks, oldest first. Keeping the history instead of
    /// one scalar lets speech bend different parts of the liquid at once.
    levels: [f32; VOICE_SAMPLES],
    colour: u32,
}

/// One live orb. The worker owns no GPUI state and ends when this is dropped.
pub(crate) struct OrbAnimation {
    latest: Arc<Mutex<Arc<RenderImage>>>,
    presented: RefCell<Option<Arc<RenderImage>>>,
    input: Arc<Mutex<OrbInput>>,
    stop: Arc<AtomicBool>,
}

impl OrbAnimation {
    pub(crate) fn start(colour: u32) -> Self {
        let surface = Surface::new(ORB_PIXELS);
        let input = Arc::new(Mutex::new(OrbInput {
            recording: true,
            levels: [0.0; VOICE_SAMPLES],
            colour,
        }));
        let first = Arc::new(render(&surface, 0.0, *lock(&input), 0.0));
        let latest: Arc<Mutex<Arc<RenderImage>>> = Arc::new(Mutex::new(first));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_input = Arc::clone(&input);
        let worker_latest = Arc::clone(&latest);
        let worker_stop = Arc::clone(&stop);
        let spawned = std::thread::Builder::new()
            .name("gui-seeker-orb".into())
            .spawn(move || {
                let mut previous_frame = Instant::now();
                let mut liquid_clock = 0.0_f32;
                let mut energy = 0.0_f32;
                while !worker_stop.load(Ordering::Relaxed) {
                    let began = Instant::now();
                    let elapsed = (began - previous_frame).as_secs_f32().min(0.10);
                    previous_frame = began;
                    let input = *lock(&worker_input);
                    let target = if input.recording {
                        input.levels[VOICE_SAMPLES - 1]
                    } else {
                        0.0
                    };
                    // Speech should move the liquid on the first syllable.
                    // Release is slower so a gap between words does not snap
                    // the whole material back to its idle shape.
                    let rate = if target > energy { 0.58 } else { 0.16 };
                    energy += (target - energy) * rate;
                    // Integrate phase instead of scaling wall time. Recording
                    // silence keeps a slow viscous drift; speech accelerates
                    // it smoothly without making the whole orb churn.
                    liquid_clock += elapsed * (0.20 + energy * 0.85);
                    let next = Arc::new(render(&surface, liquid_clock, input, energy));
                    *lock(&worker_latest) = next;
                    let spent = began.elapsed();
                    if spent < FRAME_INTERVAL {
                        std::thread::sleep(FRAME_INTERVAL - spent);
                    }
                }
            });
        if let Err(error) = spawned {
            crate::logging::error(&format!("seeker orb thread did not start: {error}"));
        }
        Self {
            latest,
            presented: RefCell::new(None),
            input,
            stop,
        }
    }

    pub(crate) fn update(&self, recording: bool, levels: &[f32], colour: u32) {
        *lock(&self.input) = OrbInput {
            recording,
            levels: microphone_history(levels),
            colour,
        };
    }

    /// Return the newest texture and retire the atlas entry it replaces.
    pub(crate) fn frame(&self, cx: &mut App) -> Arc<RenderImage> {
        let next = Arc::clone(&lock(&self.latest));
        let mut presented = self.presented.borrow_mut();
        if let Some(previous) = presented.as_ref()
            && !Arc::ptr_eq(previous, &next)
        {
            cx.drop_image(Arc::clone(previous), None);
        }
        *presented = Some(Arc::clone(&next));
        next
    }

    pub(crate) fn dispose(&self, cx: &mut App) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(last) = self.presented.borrow_mut().take() {
            cx.drop_image(last, None);
        }
    }
}

impl Drop for OrbAnimation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn microphone_history(levels: &[f32]) -> [f32; VOICE_SAMPLES] {
    let mut history = [0.0; VOICE_SAMPLES];
    let shown = &levels[levels.len().saturating_sub(history.len())..];
    let offset = history.len() - shown.len();
    for (slot, peak) in history[offset..].iter_mut().zip(shown) {
        // Typical speech peaks are far below full scale. Without this curve a
        // clear 0.05 reading moved the liquid by only a few thousandths,
        // which looked identical to silence.
        *slot = (peak.clamp(0.0, 1.0) * 5.0).sqrt().clamp(0.0, 1.0);
    }
    history
}

/// Screen-space measurements shared by every animation frame.
struct Surface {
    size: u32,
    x: Vec<f32>,
    y: Vec<f32>,
    radius: Vec<f32>,
    normal_z: Vec<f32>,
}

impl Surface {
    fn new(size: u32) -> Self {
        let count = (size * size) as usize;
        let mut surface = Self {
            size,
            x: vec![0.0; count],
            y: vec![0.0; count],
            radius: vec![0.0; count],
            normal_z: vec![0.0; count],
        };
        for row in 0..size {
            let y = ((row as f32 + 0.5) / size as f32) * 2.0 - 1.0;
            for column in 0..size {
                let x = ((column as f32 + 0.5) / size as f32) * 2.0 - 1.0;
                let index = row as usize * size as usize + column as usize;
                surface.x[index] = x;
                surface.y[index] = y;
                let radius = (x * x + y * y).sqrt() / ORB_RADIUS;
                surface.radius[index] = radius;
                surface.normal_z[index] = (1.0 - radius * radius).max(0.0).sqrt();
            }
        }
        surface
    }
}

/// Render one straight-alpha BGRA frame, the layout GPUI's atlas expects.
fn render(surface: &Surface, liquid_clock: f32, input: OrbInput, energy: f32) -> RenderImage {
    let mut pixels = RgbaImage::new(surface.size, surface.size);
    let level = energy.clamp(0.0, 1.0);
    let t = liquid_clock;
    let liquid_levels = smooth_voice(input.levels);
    let accent_hue = rgb_hsv(input.colour).0;
    // Frame-wide uniforms, not per-pixel waves: the liquid rolls and breathes
    // as one body even when the room is quiet.
    let roll = (t * 0.30).sin() * (0.26 + level * 0.12);
    let (roll_sin, roll_cos) = roll.sin_cos();
    let size = surface.size;
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        let radius = surface.radius[index];
        let x = surface.x[index];
        let y = surface.y[index];
        let nx = x / ORB_RADIUS;
        let ny = y / ORB_RADIUS;

        // The surface normal's forward component, which collapses at the limb.
        // Everything that has to know how far the glass has turned away reads
        // it: the bow of the seam and the lens.
        let nz = surface.normal_z[index];

        // A spherical lens compresses what it holds towards the rim, so the
        // internal light stretches where the wall bends away (#3050). GPUI
        // does not hand this image the real backdrop, so these coordinates
        // refract the material, not the text painted behind the window.
        let lens_x = nx * roll_cos - ny * roll_sin;
        let lens_y = nx * roll_sin + ny * roll_cos;
        let denominator = 0.22 + 0.78 * nz;
        let u = lens_x / denominator;
        let v = lens_y / denominator;
        let voice = voice_at(&liquid_levels, (u * 0.40 + 0.5).clamp(0.0, 1.0));
        let envelope = (1.0 - nx * nx).max(0.0).sqrt();
        let bend = (voice - level * 0.40) * envelope * 0.34;
        // Three travelling waves at different angles and speeds. They are hue
        // only: they move where the colour is sampled and never add light, so
        // the ribbons stay under the surface instead of painting over it.
        let ribbon = 0.52 * (0.5 + 0.5 * sine(u * 1.45 + v * 0.90 + t * 0.28))
            + 0.30 * (0.5 + 0.5 * sine(v * 2.05 - u * 1.00 - t * 0.22))
            + 0.18 * (0.5 + 0.5 * sine((u + bend) * 3.10 + t * 0.16));

        // How far this pixel sits inside the silhouette, which is the only
        // measure the border terms need: the ring lives ON the edge, so its
        // coordinate is distance from the edge and not from the centre.
        let depth = (1.0 - radius).max(0.0);

        // The horizon is the seam between a cool dome and a warm bowl, and it
        // bows towards the wall through the same lens the ribbons ride. It
        // gives the marble a top and a bottom, which is the difference between
        // an object and a lit plate.
        let horizon = HORIZON + BOW_BOWL * (1.0 - nz) + BREATHE * sine(t * 0.70);
        // The band rides a slightly different seam from the bowl's: it is the
        // horizon reflected off the inside of the sphere, so it rises towards
        // the wall while the bowl's own boundary falls away.
        let seam = horizon - BAND_ARCH * (1.0 - nz);
        let offset = ny - seam;
        // The falloff keeps its spherical shape but no longer runs to nothing at
        // the limb, which is what left the glow short of both inner walls.
        let lateral = BAND_WALL + (1.0 - BAND_WALL) * gaussian(nx, BAND_REACH);
        let band = gaussian(offset, BAND_SIGMA) * lateral;
        // The halo carries the only asymmetry in the light: the key is upper
        // left, so the halo is pushed that way and the seam's brightest point
        // sits left of centre. Nothing else in the material is directional, and
        // a field that is a pure surface of revolution reads as a flat graphic
        // however good its profile is.
        let glow = gaussian(offset, GLOW_SIGMA) * (1.0 + SIDE_BIAS * -nx) * lateral;
        let bowl = smooth(horizon, horizon + 0.60, ny);
        let dome = smooth(-0.04, 0.04, -ny);

        // The light field itself: how bright this pixel is, nought to one. The
        // border is three terms: the shoulder the glass shows as it turns away,
        // the specular ring that sits on the silhouette, and the overhead key's
        // own brighter catch on the top arc. There is no second ring further in
        // and no dark trough, because the reference has neither: sampling its
        // own edge radially gives 63 luma on the ring's first pixel, 88 at its
        // peak, then 39 and 28 on the way in.
        // The ring is keyed to the upper left, where the reference's is bright.
        let rim_key = (0.5 - 0.5 * (nx * RIM_TILT_X + ny * RIM_TILT_Y)).max(0.0);
        let rim_ring = gaussian(radius - RIM_RADIUS, RIM_WIDTH);
        // The overhead catch: the same silhouette, a narrower profile, and a key
        // that only exists between ten o'clock and two.
        let rim_arc = smooth(RIM_TOP_EDGE, RIM_TOP_FULL, ny);
        let rim_top = gaussian(radius - RIM_RADIUS, RIM_TOP_WIDTH) * rim_arc;
        // The softbox is an arc, so it is placed by radius and bearing: a ring
        // cut by angle, not a disc around one point.
        let spec_radius = (nx * nx + ny * ny).sqrt().max(1e-4);
        let spec_bearing = (nx * SPEC_DIR_X + ny * SPEC_DIR_Y) / spec_radius;
        let catch = gaussian(spec_radius - SPEC_RADIUS, SPEC_WIDTH)
            * smooth(SPEC_SPAN_LOW, SPEC_SPAN_HIGH, spec_bearing);
        let heat = (AMBIENT
            + BAND_GAIN * band
            + GLOW_GAIN * glow
            + BOWL_LIFT * bowl
            + RIM_FALL_GAIN * (-depth / RIM_FALL_SIGMA).exp()
            + RIM_GAIN * rim_ring * (RIM_BASE + (1.0 - RIM_BASE) * rim_key)
            + RIM_TOP_GAIN * rim_top
            + FAR_WALL * gaussian(ny - (horizon + FAR_LIFT), GLOW_SIGMA * 0.85)
            + SPEC_GAIN * catch)
            .clamp(0.0, 1.0);

        // Two colours of equal brightness, crossfaded. Interpolating the hue
        // instead would walk from the warm side to the cool one through green,
        // and the band is exactly where both masks overlap, so the core of the
        // light came out green. Crossfading in light space cannot do that.
        let saturation = saturation_at(heat);
        let fringe = DISPERSION * depth.powi(6);
        let warm_hue = BOWL_HUE + (accent_hue - AMBER_HUE) * WARM_FOLLOW;
        let warm = tinted(warm_hue + (ribbon - 0.5) * 0.10 - fringe, saturation, heat);
        let cool = tinted(COOL_HUE + fringe, saturation, heat);
        let mut red = warm.0 * (1.0 - dome) + cool.0 * dome;
        let mut green = warm.1 * (1.0 - dome) + cool.1 * dome;
        let mut blue = warm.2 * (1.0 - dome) + cool.2 * dome;
        // A specular that bright has lost most of its hue. Pulling the overhead
        // catch towards white is what makes it read as silver rather than as a
        // lit cyan edge; the composite correction below restores the luma, so
        // this moves colour only.
        let silver = (RIM_SILVER * rim_top).clamp(0.0, 1.0);
        red += (1.0 - red) * silver;
        green += (1.0 - green) * silver;
        blue += (1.0 - blue) * silver;
        // How much of the page this pixel lets through. The body is smoke and
        // the hot events are dense, which is the difference between glass and a
        // painting of glass.
        let glass = GLASS_FLOOR + (1.0 - GLASS_FLOOR) * heat;

        // The fitted values are the reference's *composited* pixels, so over the
        // dark scrim the emitted colour has to be lifted to land on them:
        // solving `over = emit * alpha + scrim * (1 - alpha)` for `emit` is what
        // keeps the translucency from simply darkening the object. Without this
        // the marble came out a third too dark and no amount of extra light in
        // the field fixed it, because the field was already correct.
        if glass > 1e-4 {
            let emitted = relative_luma(red, green, blue);
            if emitted > 1e-4 {
                let composited = (heat - SCRIM_LUMA * (1.0 - glass)) / glass;
                let gain = (composited.max(0.0) / emitted).min(MAX_LIFT);
                red = (red * gain).min(1.0);
                green = (green * gain).min(1.0);
                blue = (blue * gain).min(1.0);
            }
        }

        // A hard circular silhouette with a one-pixel anti-aliased ramp. The
        // body no longer feathers to nothing across the whole frame, so the
        // image's own square boundary cannot paint a wash over the scrim it is
        // composited onto (#3085). The margin left by `ORB_RADIUS` is what the
        // ramp has to run in.
        //
        // The ramp saturates at one inside the silhouette, and it has to be
        // clamped BEFORE it meets the glass, not after. Multiplying first turns
        // a ramp that is thirty-plus deep in the body into a clamp that pins
        // every inner pixel to fully opaque, which is exactly how the glass
        // ended up with no transmission at all.
        let edge = ((1.0 - radius) * ORB_RADIUS * size as f32 * 0.5).clamp(0.0, 1.0);
        let alpha = (glass * edge).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            continue;
        }
        // Ordered dither plus a little hash noise. The body is a slow gradient
        // painted into 8 bits, and without this it bands in the dark outer
        // third where the ramp's stops sit closest together.
        let row = index as u32 / size;
        let column = index as u32 % size;
        let dither = ((BAYER[((row & 3) * 4 + (column & 3)) as usize] - 0.5) * 1.4
            + (hash_noise(index) - 0.5) * 0.8)
            / 255.0;
        *pixel = bgra(red + dither, green + dither, blue + dither, alpha + dither);
    }
    RenderImage::new(vec![Frame::new(pixels)])
}

/// Low-pass the meter before it deforms the liquid. Individual meter bars
/// remain visible to speech, but their sharp edges do not become sound-wave
/// spikes in the glass.
fn smooth_voice(levels: [f32; VOICE_SAMPLES]) -> [f32; VOICE_SAMPLES] {
    let mut smoothed = [0.0; VOICE_SAMPLES];
    for (index, output) in smoothed.iter_mut().enumerate() {
        let mut sum = 0.0;
        for (tap, weight) in [1.0_f32, 2.0, 3.0, 2.0, 1.0].into_iter().enumerate() {
            let sample = (index as isize + tap as isize - 2).clamp(0, VOICE_SAMPLES as isize - 1);
            sum += levels[sample as usize] * weight;
        }
        *output = sum / 9.0;
    }
    smoothed
}

/// Sample the scrolling meter at a horizontal position inside the sphere.
fn voice_at(levels: &[f32; VOICE_SAMPLES], position: f32) -> f32 {
    let position = position.clamp(0.0, 1.0) * (levels.len() - 1) as f32;
    let index = position as usize;
    let next = (index + 1).min(levels.len() - 1);
    let fraction = position - index as f32;
    levels[index] * (1.0 - fraction) + levels[next] * fraction
}

/// A Gaussian ring, which is what the thin bands of the material are built
/// from.
///
/// A linear falloff would end exactly at `width`, and a band whose value
/// reaches zero at a fixed radius prints a faint ring of its own. A Gaussian
/// has no edge to print.
fn gaussian(value: f32, width: f32) -> f32 {
    let scaled = value / width;
    (-scaled * scaled).exp()
}

/// Saturation for a given brightness, read off the reference.
fn saturation_at(heat: f32) -> f32 {
    if heat <= HEAT_STOPS[0] {
        return SAT_STOPS[0];
    }
    for index in 0..HEAT_STOPS.len() - 1 {
        let (low, high) = (HEAT_STOPS[index], HEAT_STOPS[index + 1]);
        if heat <= high {
            let span = high - low;
            let k = if span <= 0.0 {
                0.0
            } else {
                (heat - low) / span
            };
            return SAT_STOPS[index] * (1.0 - k) + SAT_STOPS[index + 1] * k;
        }
    }
    SAT_STOPS[SAT_STOPS.len() - 1]
}

/// A colour of the requested hue and saturation, scaled so its luma is exactly
/// `heat`.
///
/// Scaling by luma is the whole trick: it lets brightness be decided once, by
/// the light field, and colour be decided independently. Mixing the two is what
/// made every theme's orb a solid gold ball.
fn tinted(hue: f32, saturation: f32, heat: f32) -> (f32, f32, f32) {
    let base = hsv_rgb(hue, saturation, 1.0);
    let brightness = relative_luma(base.0, base.1, base.2);
    if brightness < 1e-4 {
        return (heat, heat, heat);
    }
    let scale = heat / brightness;
    (
        (base.0 * scale).min(1.0),
        (base.1 * scale).min(1.0),
        (base.2 * scale).min(1.0),
    )
}

/// Rec. 709 relative luma, the weights the reference's own values assume.
fn relative_luma(red: f32, green: f32, blue: f32) -> f32 {
    0.2126 * red + 0.7152 * green + 0.0722 * blue
}

/// Accent colour to HSV, so the warm side can lean towards the theme.
fn rgb_hsv(colour: u32) -> (f32, f32, f32) {
    let (red, green, blue) = rgb(colour);
    let high = red.max(green).max(blue);
    if high <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let low = red.min(green).min(blue);
    let span = high - low;
    if span <= 0.0 {
        return (0.0, 0.0, high);
    }
    let hue = if high == red {
        ((green - blue) / span) / 6.0
    } else if high == green {
        (2.0 + (blue - red) / span) / 6.0
    } else {
        (4.0 + (red - green) / span) / 6.0
    };
    (hue.rem_euclid(1.0), span / high, high)
}

/// HSV to RGB with the hue wrapped, the inverse of `rgb_hsv`.
fn hsv_rgb(hue: f32, saturation: f32, value: f32) -> (f32, f32, f32) {
    let position = hue.rem_euclid(1.0) * 6.0;
    let index = (position as usize).min(5);
    let fraction = position - index as f32;
    let saturation = saturation.clamp(0.0, 1.0);
    let value = value.clamp(0.0, 1.0);
    let floor = value * (1.0 - saturation);
    let falling = value * (1.0 - saturation * fraction);
    let rising = value * (1.0 - saturation * (1.0 - fraction));
    match index {
        0 => (value, rising, floor),
        1 => (falling, value, floor),
        2 => (floor, value, rising),
        3 => (floor, falling, value),
        4 => (rising, floor, value),
        _ => (value, floor, falling),
    }
}

/// A deterministic per-pixel hash for the dither.
///
/// Ordered dither alone leaves a visible 4x4 lattice on a gradient this slow,
/// and the hash noise breaks that lattice up without moving between frames of
/// the same instant.
fn hash_noise(index: usize) -> f32 {
    let mut value = index.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff;
    value ^= value >> 13;
    (value & 0xffff) as f32 / 65_535.0
}

fn smooth(from: f32, to: f32, value: f32) -> f32 {
    bell_helper(((value - from) / (to - from)).clamp(0.0, 1.0))
}

fn bell_helper(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Interpolated sine table: continuous motion without millions of libm calls.
fn sine(value: f32) -> f32 {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=SINE_STEPS)
            .map(|index| (index as f32 * TAU / SINE_STEPS as f32).sin())
            .collect()
    });
    let position = (value / TAU).rem_euclid(1.0) * SINE_STEPS as f32;
    let index = position as usize;
    let fraction = position - index as f32;
    // Rounding can put a value just below one full turn on the table's end.
    // Wrap both lookups so interpolation never indexes past the allocation.
    let from = index % SINE_STEPS;
    let to = (from + 1) % SINE_STEPS;
    table[from] * (1.0 - fraction) + table[to] * fraction
}

fn rgb(colour: u32) -> (f32, f32, f32) {
    (
        ((colour >> 16) & 0xff) as f32 / 255.0,
        ((colour >> 8) & 0xff) as f32 / 255.0,
        (colour & 0xff) as f32 / 255.0,
    )
}

fn bgra(red: f32, green: f32, blue: f32, alpha: f32) -> Rgba<u8> {
    Rgba([byte(blue), byte(green), byte(red), byte(alpha)])
}

fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
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

    #[test]
    fn the_orb_is_sized_to_its_bubble_and_actually_transparent() {
        // The bubble is 9 rem of a 16 px rem, so 288 device pixels on Retina.
        // Anything above that is work no eye can see.
        assert_eq!(ORB_PIXELS, 320);
        let surface = Surface::new(96);
        let frame = render(
            &surface,
            0.0,
            OrbInput {
                recording: true,
                levels: [0.4; VOICE_SAMPLES],
                colour: 0xffbf47,
            },
            0.4,
        );
        assert_eq!(frame.size(0).width.0, 96);
        assert_eq!(frame.size(0).height.0, 96);
        let bytes = frame.as_bytes(0).expect("one frame");
        assert_eq!(bytes[3], 0, "the corner is clear, not a black square");
        let centre = ((48 * 96 + 48) * 4) as usize;
        assert!(
            bytes[centre + 3] > 230,
            "the middle of the marble is solid, not a see-through lens"
        );
    }

    /// The frame the structural tests share: one frame, idle level, no speech.
    fn glass_frame(size: u32) -> gpui::RenderImage {
        let surface = Surface::new(size);
        render(
            &surface,
            0.2,
            OrbInput {
                recording: true,
                levels: [0.25; VOICE_SAMPLES],
                colour: 0xffbf47,
            },
            0.3,
        )
    }

    #[test]
    fn the_orb_stops_at_a_hard_round_edge_and_leaves_the_frame_clear() {
        // The shipped lens feathered to nothing across the whole quad, so the
        // image's square boundary painted a wash over the scrim and the user
        // saw a rectangle around the orb. A marble has a silhouette instead:
        // a hard circular boundary with a ramp narrow enough to read as an
        // edge rather than as a fade. The body is translucent by design now, so
        // the silhouette is measured by where the frame starts carrying
        // anything at all, not by where it becomes fully opaque.
        let size = 160;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        assert!(
            sample(bytes, size, 0.0, 0.0).3 > 200,
            "the middle of the marble is dense"
        );
        assert!(
            sample(bytes, size, 0.90, 0.0).3 > 0,
            "the body still carries density close to the edge"
        );
        assert_eq!(
            sample(bytes, size, 1.02, 0.0).3,
            0,
            "one step past the radius the frame carries nothing"
        );
        assert_eq!(
            sample(bytes, size, 0.99, 0.99).3,
            0,
            "the frame's corners are clear, not a wash"
        );
        // Walking in along the centre row, the first pixel that carries
        // anything must be at the radius itself: a wider approach means the
        // silhouette is fading rather than ending.
        let row = size as usize / 2;
        let strip = &bytes[row * size as usize * 4..(row + 1) * size as usize * 4];
        let first = strip
            .as_chunks::<4>()
            .0
            .iter()
            .position(|pixel| pixel[3] > 0)
            .expect("the orb reaches the centre row");
        let expected = ((1.0 - ORB_RADIUS) * 0.5 * size as f32) as usize;
        assert!(
            first.abs_diff(expected) <= 2,
            "the silhouette must begin at the radius, expected about {expected}, got {first}"
        );
    }

    #[test]
    fn the_border_is_one_thin_ring_with_an_exponential_shoulder() {
        // The border is one hard event and nothing else. Sampling the
        // reference radially from its centre gives 88 luma at the ring's peak
        // on the silhouette, 39 two pixels inside it, 28 five pixels inside and
        // 18 in the body. There is no second bright ring further in and no dark
        // trough between them: an earlier pass invented a bevel at r 0.80 and a
        // 0.34-deep trough at r 0.90, and that invented pair is what read as a
        // soft double ring.
        // The ring is narrower than a pixel at the test's default size, so this
        // reads the shipped size: at 160 the ring can fall between two pixel
        // centres and measure nothing at all.
        let size = ORB_PIXELS;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        let rim = luma(sample(bytes, size, 0.0, -RIM_RADIUS));
        let shoulder = luma(sample(bytes, size, 0.0, -0.96));
        let inside = luma(sample(bytes, size, 0.0, -0.90));
        let body = luma(sample(bytes, size, 0.0, -0.60));
        // The assertions are the contract, not a table of numbers: the rim is
        // re-measured whenever the light field changes, and a recorded sample
        // would only be a stale copy of the maths sitting next to it.
        assert!(
            rim > shoulder + 35.0,
            "the ring must beat the shoulder behind it, got {rim:.1} against {shoulder:.1}"
        );
        assert!(
            shoulder > inside,
            "the shoulder must fall away into the body, got {shoulder:.1} against {inside:.1}"
        );
        assert!(
            inside >= body - 2.0,
            "no trough sits between the shoulder and the body, got {inside:.1} and {body:.1}"
        );
    }

    #[test]
    fn the_ring_is_keyed_to_the_upper_arc() {
        // The reference's own ring measures 88 luma at the top, 64 on the left
        // and 30 at the bottom, so the key enters the glass from above. A ring
        // of even brightness is a drawn outline; a keyed one is a specular.
        let size = ORB_PIXELS;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        let top = luma(sample(bytes, size, 0.0, -RIM_RADIUS));
        let bottom = luma(sample(bytes, size, 0.0, RIM_RADIUS));
        let left = luma(sample(bytes, size, -RIM_RADIUS, 0.0));
        let right = luma(sample(bytes, size, RIM_RADIUS, 0.0));
        // The ring is one gradient plus the overhead catch, so the top sits well
        // clear of the bottom by construction; this pins that it does.
        assert!(
            top > bottom + 30.0,
            "the ring must be brighter above than below, got {top:.1} against {bottom:.1}"
        );
        assert!(
            left > right + 30.0,
            "the ring must be brighter on the left than the right, got {left:.1} against {right:.1}"
        );
    }

    #[test]
    fn the_horizon_glow_reaches_both_inner_walls() {
        // The band used to fall to a fifth of its centre brightness by the
        // silhouette, which left a dark gap between the end of the glow and the
        // glass it is meant to be inside. `BAND_WALL` holds a floor at the limb,
        // so the strip still has to be lit where it meets the wall.
        let size = ORB_PIXELS;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        let wall = luma(sample(bytes, size, 0.94, -0.05));
        let body = luma(sample(bytes, size, 0.0, -0.60));
        assert!(
            wall > body + 40.0,
            "the glow must still be lit at the wall, got {wall:.1} against a body at {body:.1}"
        );
    }

    #[test]
    fn the_softbox_is_an_arc_not_a_dot() {
        // A point-and-sigma catch lights one spot and nothing either side of it.
        // The reference's upper-left reflection is a slab that follows the
        // contour, so both ends of the arc have to carry light, not just its
        // middle.
        let size = ORB_PIXELS;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        let body = luma(sample(bytes, size, 0.0, -0.60));
        let low = luma(sample(bytes, size, -0.053, -0.758));
        let high = luma(sample(bytes, size, -0.699, -0.298));
        assert!(
            low > body + 20.0,
            "the arc's upper end must carry light, got {low:.1} against a body at {body:.1}"
        );
        assert!(
            high > body + 20.0,
            "the arc's lower end must carry light, got {high:.1} against a body at {body:.1}"
        );
    }

    #[test]
    fn the_horizon_splits_a_cool_dome_from_a_warm_bowl() {
        // One radial gradient is a lit plate no matter how smooth it is. The
        // object gains a top and a bottom from a temperature split across a
        // thin band of light: cool above, warm below, hotspot on the seam.
        let size = 160;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        let dome = sample(bytes, size, 0.0, -0.20);
        let band = luma(sample(bytes, size, 0.0, 0.02));
        let bowl = sample(bytes, size, 0.0, 0.20);
        // Measured: dome blue leads red by 43 of 255, bowl red leads blue by
        // 51, band luma 255 against a dome at 121 and a bowl at 130.
        assert!(
            dome.2 - dome.0 > 15.0,
            "the dome must run cool, got blue minus red of {:.1}",
            dome.2 - dome.0
        );
        assert!(
            bowl.0 - bowl.2 > 15.0,
            "the bowl must keep the accent's warmth, got red minus blue of {:.1}",
            bowl.0 - bowl.2
        );
        assert!(
            band > luma(dome) + 80.0,
            "the horizon band is the brightest thing in the frame, got {band:.1} against {:.1}",
            luma(dome)
        );
        assert!(
            band > luma(bowl) + 80.0,
            "the band must also outshine the bowl under it, got {band:.1} against {:.1}",
            luma(bowl)
        );
    }

    #[test]
    fn the_body_passes_the_page_through_and_only_the_light_is_dense() {
        // Glass over a page, not a painting of glass. The dark body is smoke:
        // it lets a large part of the backdrop through, and only the band and
        // the rim go dense. Pinned as alpha, because alpha is exactly what the
        // compositor uses to decide how much of the text behind reaches the
        // user's eye.
        let size = 160;
        let frame = glass_frame(size);
        let bytes = frame.as_bytes(0).expect("one frame");
        let body = sample(bytes, size, 0.0, -0.60).3;
        let band = sample(bytes, size, 0.0, 0.02).3;
        // Measured 53 in the body and 252 on the band.
        assert!(
            body < 140,
            "the dark body must stay translucent, got alpha {body}"
        );
        assert!(body > 40, "the body must still carry density, got {body}");
        assert!(
            band > 240,
            "the band is the one dense event in the material, got alpha {band}"
        );
    }

    #[test]
    fn the_orb_moves_and_answers_the_microphone() {
        // Motion here is a slow drift of the liquid, not a spin, so the bar is
        // a few levels spread over many pixels rather than a few pixels at a
        // large delta. The dither is fixed per pixel, so it cancels between two
        // frames and any difference measured here is the material moving.
        let surface = Surface::new(96);
        let input = OrbInput {
            recording: true,
            levels: [0.2; VOICE_SAMPLES],
            colour: 0xffbf47,
        };
        let first = render(&surface, 0.2, input, 0.2);
        let later = render(&surface, 0.8, input, 0.2);
        let before = first.as_bytes(0).expect("first frame");
        let after = later.as_bytes(0).expect("later frame");
        let moved: Vec<usize> = before
            .iter()
            .zip(after)
            .map(|(a, b)| a.abs_diff(*b) as usize)
            .collect();
        let visibly_changed = moved.iter().filter(|shift| **shift > 2).count();
        assert!(
            visibly_changed > before.len() / 200,
            "motion must reshape visible pixels, not merely change one sample: \
             {visibly_changed} of {} moved",
            before.len()
        );
        assert!(
            moved.iter().max().copied().unwrap_or(0) > 4,
            "no pixel moved far enough to read as motion"
        );
        let mut spoken = input;
        for (index, level) in spoken.levels.iter_mut().enumerate() {
            *level = if index % 2 == 0 { 0.05 } else { 0.95 };
        }
        let louder = render(&surface, 0.2, spoken, 0.9);
        let speech_changed = before
            .iter()
            .zip(louder.as_bytes(0).expect("louder frame"))
            .filter(|(a, b)| a.abs_diff(**b) > 2)
            .count();
        assert!(
            speech_changed > before.len() / 200,
            "the recorded waveform must visibly reshape the liquid"
        );
    }

    #[test]
    fn normal_speech_is_amplified_and_newest_on_the_right() {
        let history = microphone_history(&[0.05, 0.2]);
        assert!(history[..history.len() - 2].iter().all(|peak| *peak == 0.0));
        assert!((history[history.len() - 2] - 0.5).abs() < 0.001);
        assert!((history[history.len() - 1] - 1.0).abs() < 0.001);
        assert_eq!(voice_at(&history, 1.0), 1.0);
    }

    #[test]
    fn sine_wraps_when_rounding_reaches_the_table_end() {
        // The largest float below one full turn, times the table size, lands a
        // hair under the last step. The lookup takes that final cell and
        // interpolates toward the wrapped first one instead of indexing past
        // the allocation.
        let almost_one_turn = f32::from_bits(0x3f7f_ffff);
        let position = almost_one_turn * SINE_STEPS as f32;
        assert_eq!(position as usize, SINE_STEPS - 1);
        assert!(sine(TAU * almost_one_turn).abs() < 0.001);
    }
}
