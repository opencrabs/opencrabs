//! Procedural Siri-style glass for the Seeker's recording bubble (#3044).
//!
//! GPUI cannot sample and refract the window behind one element, so this draws
//! the material itself: one tonal field moved by dodge and burn, one emissive
//! ramp derived from the accent colour, and a lamp for the specular core. A
//! worker produces the frames and the UI only uploads the newest one, keeping
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
const ORB_RADIUS: f32 = 0.92;
const SINE_STEPS: usize = 4096;
const VOICE_SAMPLES: usize = super::voice_input::METER_BARS;

/// The key light: upper left, and forward enough that the terminator crosses
/// the body instead of falling outside it.
///
/// The forward component alone decides where the terminator lands. For a
/// sphere it crosses at `radius = KEY[2]`, so a light pushed much past 0.8
/// leaves every visible pixel lit and the orb reads as a flat plate no matter
/// how good the falloff is. The shipped 1024 px lens had no light direction at
/// all, which is why its two sides differed by 0.002 alpha.
const KEY: [f32; 3] = [-0.60, -0.36, 0.72];

/// Where the dodge starts and finishes on `lambert`.
///
/// The top of this range is the part that makes or breaks the figure. A dodge
/// that saturates early is full over most of the visible disc, the heat clips,
/// and the ramp returns its near-white stop across a wide area: a lit plate.
/// Pushing the finish past the mirror point leaves a gradient across the whole
/// lit half while the range still bottoms out below the terminator.
const DODGE_FROM: f32 = -0.10;
const DODGE_TO: f32 = 0.62;
const HEAT_BASE: f32 = 0.10;
const DODGE_GAIN: f32 = 0.72;
const CORE_DEPTH: f32 = 0.30;
const BOUNCE_LIFT: f32 = 0.16;
const LAMP_GAIN: f32 = 0.14;

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
    let ramp = orb_ramp(input.colour);
    let flat = (KEY[0] * KEY[0] + KEY[1] * KEY[1]).sqrt();
    let face_x = KEY[0] / flat;
    let face_y = KEY[1] / flat;
    // Frame-wide uniforms, not per-pixel waves: the liquid rolls and breathes
    // as one body even when the room is quiet.
    let roll = (t * 0.30).sin() * (0.26 + level * 0.12);
    let (roll_sin, roll_cos) = roll.sin_cos();
    let breathe = 0.5 + 0.5 * (t * 0.85).sin();
    // The bloom sits just off centre along the key direction, so it is never
    // concentric with the body it lies on. A luminous marble carries no jewel
    // specular: the lamp is a soft bloom near the middle, and the direction is
    // read from the body's dodge rather than from where the highlight sits.
    let push = (0.15 + 0.06 * level) * ORB_RADIUS;
    let size = surface.size;
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        let radius = surface.radius[index];
        let x = surface.x[index];
        let y = surface.y[index];
        let nx = x / ORB_RADIUS;
        let ny = y / ORB_RADIUS;

        // The body is light leaving the sphere rather than a disc with an
        // outline, so its density follows a flat-topped curve with a long
        // tail. That tail is the bloom, and it is what lets the orb sit on the
        // scrim without a drawn edge (#3085).
        let body = quartic(radius, 0.62);
        let lamp_x = x - face_x * push;
        let lamp_y = y - face_y * push;
        let lamp = quartic((lamp_x * lamp_x + lamp_y * lamp_y).sqrt() / ORB_RADIUS, 0.16);

        // A real surface normal, not a flat direction. `nz` collapses at the
        // limb, so the far side turns out of the key instead of merely fading.
        // A flat direction leaves the falloff identical in every direction,
        // which is what read as a plate with a gradient painted on it.
        let nz = surface.normal_z[index];
        let lambert = nx * KEY[0] + ny * KEY[1] + nz * KEY[2];

        // The five values a retoucher builds by hand: a lit side, a terminator
        // that actually turns, a core shadow just past it, and a bounce at the
        // far limb. Smoothing this away was the mistake worth undoing: the eye
        // reads volume off the edges between value zones, never off the values
        // themselves.
        let dodge = smooth(DODGE_FROM, DODGE_TO, lambert);
        let core_shadow = bell(lambert + 0.34, 0.30);
        let bounce = bell(lambert + 0.86, 0.26);

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
        // only: they move where the ramp gets sampled and never add light, so
        // the ribbons stay under the surface instead of painting over it.
        let ribbon = 0.52 * (0.5 + 0.5 * sine(u * 1.45 + v * 0.90 + t * 0.28))
            + 0.30 * (0.5 + 0.5 * sine(v * 2.05 - u * 1.00 - t * 0.22))
            + 0.18 * (0.5 + 0.5 * sine((u + bend) * 3.10 + t * 0.16));

        let heat = (body
            * (HEAT_BASE + DODGE_GAIN * dodge - CORE_DEPTH * core_shadow
                + BOUNCE_LIFT * bounce
                + (ribbon - 0.5) * 0.13)
            + LAMP_GAIN * lamp
            + 0.03 * breathe)
            .clamp(0.04, 1.0);
        let (mut red, mut green, mut blue) = ramp_sample(&ramp, heat);

        // The bloom lifts the core towards white on its own curve instead of
        // inheriting the body's ramp, so the centre stays luminous while the
        // surrounding hue keeps its saturation.
        let toe = lamp * lamp * lamp * 0.62;
        red += (1.00 - red) * toe;
        green += (0.99 - green) * toe;
        blue += (0.96 - blue) * toe;

        // The tail is the bloom and it reaches the corners of the frame. A
        // soft window closes it, so the image carries a clear margin instead
        // of painting a square wash over the scrim it is composited onto.
        let halo = 1.0 - smooth(1.05, 1.45, radius);
        let alpha = ((body * (1.28 + 0.10 * level) + 0.03) * halo).clamp(0.0, 1.0);
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
        *pixel = bgra(
            red + dither,
            green + dither,
            blue + dither,
            alpha + dither,
        );
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

/// Compact, smooth bell curve without a per-pixel exponential call.
fn bell(value: f32, width: f32) -> f32 {
    let t = (1.0 - value.abs() / width).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 1/(1 + (value/scale)^4): the cheap stand-in for a super-gaussian.
///
/// Two of these run per pixel per frame, and the generic `powf` they replace
/// cost more than the rest of the shader put together. The shape is what the
/// orb needs: a flat top over the core, a soft shoulder, and a long tail with
/// no boundary that could read as a stroke.
fn quartic(value: f32, scale: f32) -> f32 {
    let scaled = value / scale;
    let squared = scaled * scaled;
    1.0 / (1.0 + squared * squared)
}

/// Six emissive stops built once a frame from the accent colour.
///
/// The orb has to follow the theme, so the ramp is derived from the accent hue
/// rather than hardcoding one palette. The hue walks warmer as the value drops
/// and cooler as it climbs, and saturation only falls at the very top: a dim,
/// desaturated stop is precisely what turns amber into khaki, which is what an
/// earlier pass kept doing.
fn orb_ramp(colour: u32) -> [(f32, f32, f32); 6] {
    let (hue, saturation, value) = rgb_hsv(colour);
    [
        hsv_rgb(hue - 0.034, saturation * 1.38, value * 0.40),
        hsv_rgb(hue - 0.022, saturation * 1.30, value * 0.60),
        hsv_rgb(hue - 0.009, saturation * 1.12, value * 0.80),
        hsv_rgb(hue + 0.005, saturation * 0.92, value * 0.94),
        hsv_rgb(hue + 0.021, saturation * 0.60, value * 1.00),
        hsv_rgb(hue + 0.037, saturation * 0.14, value * 1.00),
    ]
}

/// Where each ramp stop sits on the heat axis.
///
/// The stops are not evenly spaced. More than half the ramp sits below the
/// midpoint, because the terminator and the core shadow are what carry the
/// form and they live down there. Sampling on even fifths drags the warm stops
/// into the shadow and greys the sphere.
const RAMP_STOPS: [f32; 6] = [0.00, 0.22, 0.44, 0.66, 0.84, 1.00];

/// Sample the ramp between its stops.
///
/// The blend runs on the squares and then takes the root. A straight linear
/// blend between two emissive stops greys the midpoint, and the ramp is the one
/// place the orb cannot afford to grey.
fn ramp_sample(ramp: &[(f32, f32, f32); 6], position: f32) -> (f32, f32, f32) {
    let position = position.clamp(0.0, 1.0);
    if position <= RAMP_STOPS[0] {
        return ramp[0];
    }
    for index in 0..5 {
        let (low, high) = (RAMP_STOPS[index], RAMP_STOPS[index + 1]);
        if position <= high {
            let span = high - low;
            let k = if span <= 0.0 {
                0.0
            } else {
                (position - low) / span
            };
            let mix =
                |from: f32, to: f32| (from * from * (1.0 - k) + to * to * k).max(0.0).sqrt();
            return (
                mix(ramp[index].0, ramp[index + 1].0),
                mix(ramp[index].1, ramp[index + 1].1),
                mix(ramp[index].2, ramp[index + 1].2),
            );
        }
    }
    ramp[5]
}

/// Accent colour to HSV, so the ramp can walk the hue around it.
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

    #[test]
    fn the_light_comes_from_the_upper_left_and_the_far_side_turns_away() {
        // The whole point of the rebuild: a real light direction. The shipped
        // lens differed by 0.002 alpha between its two sides, which is why it
        // read as a plate with a gradient rather than as a lit volume.
        let size = 160;
        let surface = Surface::new(size);
        let frame = render(
            &surface,
            0.2,
            OrbInput {
                recording: true,
                levels: [0.25; VOICE_SAMPLES],
                colour: 0xffbf47,
            },
            0.3,
        );
        let bytes = frame.as_bytes(0).expect("one frame");
        let left = luma(sample(bytes, size, -0.5, 0.0));
        let right = luma(sample(bytes, size, 0.5, 0.0));
        assert!(
            left > right + 25.0,
            "the key side must outshine the away side, got {left:.1} against {right:.1}"
        );
        let upper = luma(sample(bytes, size, -0.45, -0.35));
        let lower = luma(sample(bytes, size, 0.45, 0.35));
        assert!(
            upper > lower + 30.0,
            "the key is upper left, so that quadrant must be the lit one, got \
             {upper:.1} against {lower:.1}"
        );
    }

    #[test]
    fn the_ramp_stays_in_the_accent_hue_family() {
        // Amber is the shipped accent. A ramp that drifts into the yellow-green
        // gap turns the dim end khaki, which is the failure this pins: an
        // earlier pass darkened a pale stop and got khaki instead of amber.
        let ramp = orb_ramp(0xffbf47);
        for (index, stop) in ramp.iter().enumerate() {
            let hue = hue_of(*stop);
            assert!(
                (0.06..0.16).contains(&hue),
                "ramp stop {index} drifted to hue {hue:.3}, outside the amber family"
            );
        }
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

    /// Hue of a colour in turns, so the ramp test can say where a stop landed
    /// without going through the packed accent type.
    fn hue_of(colour: (f32, f32, f32)) -> f32 {
        let high = colour.0.max(colour.1).max(colour.2);
        let low = colour.0.min(colour.1).min(colour.2);
        let span = high - low;
        if span <= 0.0 {
            return 0.0;
        }
        let raw = if high == colour.0 {
            ((colour.1 - colour.2) / span) / 6.0
        } else if high == colour.1 {
            (2.0 + (colour.2 - colour.0) / span) / 6.0
        } else {
            (4.0 + (colour.0 - colour.1) / span) / 6.0
        };
        raw.rem_euclid(1.0)
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
