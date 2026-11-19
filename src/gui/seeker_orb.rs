//! Procedural Siri-style glass for the Seeker's recording bubble (#3044).
//!
//! GPUI cannot sample and refract the window behind one element, so this draws
//! the glass material itself: a translucent volume, moving liquid waves, a
//! Fresnel rim and specular light. A worker produces 1024 px RGBA frames and
//! the UI only uploads the newest one, keeping pixel math off the UI thread.

use gpui::{App, RenderImage};
use image::{Frame, Rgba, RgbaImage};
use std::cell::RefCell;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Enough pixels for a Retina-sized bubble at more than 2x its painted size.
pub(crate) const ORB_PIXELS: u32 = 1024;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const ORB_RADIUS: f32 = 0.92;
const SINE_STEPS: usize = 4096;
const VOICE_SAMPLES: usize = super::voice_input::METER_BARS;

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
    let tint = rgb(input.colour);
    let level = energy.clamp(0.0, 1.0);
    let t = liquid_clock;
    let liquid_levels = smooth_voice(input.levels);
    // These are frame-wide uniforms, not per-pixel waves: the whole body of
    // liquid visibly rolls and breathes even when the room is quiet.
    let roll = (t * 0.32).sin() * (0.18 + level * 0.08);
    let (roll_sin, roll_cos) = roll.sin_cos();
    let sway = (t * 0.72).sin();
    let breathe = 0.5 + 0.5 * (t * 0.90).sin();
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        let radius = surface.radius[index];
        if radius > 1.18 {
            continue;
        }
        let x = surface.x[index];
        let y = surface.y[index];
        if radius > 1.0 {
            // Offset the penumbra below the lens so the transparent margin
            // reads as a cast shadow, not a uniformly dark circular outline.
            let drop_radius = (x * x + (y - 0.10) * (y - 0.10)).sqrt() / ORB_RADIUS;
            let shadow = diffuse(drop_radius - 1.04, 0.13) * 0.30;
            let glow = diffuse(radius - 1.0, 0.10) * (0.018 + level * 0.025);
            *pixel = bgra(
                tint.0 * 0.012,
                tint.1 * 0.016,
                tint.2 * 0.024,
                shadow + glow,
            );
            continue;
        }

        let coverage =
            ((1.0 - radius) * surface.size as f32 * ORB_RADIUS * 0.5 + 0.5).clamp(0.0, 1.0);
        let nx = x / ORB_RADIUS;
        let ny = y / ORB_RADIUS;
        let nz = surface.normal_z[index];
        let edge = 1.0 - nz;

        // A spherical lens compresses its environment towards the rim. The
        // increasing radial term makes the internal light stretch strongly at
        // the edge while the centre stays calm. GPUI does not expose the real
        // backdrop to this image, so these coordinates refract the material,
        // not the text painted behind the window (#3050).
        let lens = 0.10 + edge.powf(2.4) * 0.78;
        let turbulence = 0.010 + level * 0.018;
        let liquid_x = nx * roll_cos - ny * roll_sin;
        let liquid_y = nx * roll_sin + ny * roll_cos;
        let voice = voice_at(&liquid_levels, (liquid_x * 0.5 + 0.5).clamp(0.0, 1.0));
        let voice_envelope = (1.0 - nx * nx).max(0.0).sqrt();
        let voice_ripple = voice * voice_envelope;
        let current_x = sine(liquid_y * 1.35 + t * 0.32);
        let current_y = sine(liquid_x * 1.45 - t * 0.27);
        let voice_bend = (voice_ripple - level * voice_envelope * 0.45) * 0.050;
        let flow_x = x - nx * lens + current_x * turbulence;
        let flow_y = y - ny * lens + current_y * turbulence + voice_bend;
        let side = nx.abs().clamp(0.0, 1.0);
        let envelope = 1.0 - smooth(0.82, 1.0, radius) * 0.92;

        // A nearly level horizon carries a diffuse optical bloom, not a solid
        // arc. It reaches both sides of the lens, becoming thinner and dimmer
        // as the curved wall bends away at the outer rim (#3050).
        // Sit below centre like a refracted horizon, with only a gentle
        // spherical arch. The surrounding liquid still moves the line subtly.
        let centre_line = 0.20
            + 0.065 * side.powf(2.0)
            + sway * 0.006
            + sine(flow_x * 1.2 - t * 0.28) * (0.002 + level * 0.004);
        let centre = flow_y - centre_line;
        // Keep transmitted light continuous into both side walls. Only its
        // thickness and intensity taper; there are no detached capsule ends.
        let horizon_taper = 1.0 - smooth(0.62, 1.0, side) * 0.58;
        let horizon_span = horizon_taper;
        let bloom_width = (0.075 + level * 0.008 + breathe * 0.004) * (0.56 + horizon_taper * 0.44);
        let horizon = diffuse(centre, bloom_width * 1.65) * horizon_span;
        // A narrow radial white core feathers into the transmitted colour,
        // rather than resolving into a thick capsule across the glass.
        let white_hotspot = 1.0 - smooth(0.12, 0.40, side);
        let white = diffuse(centre, bloom_width * 0.42) * horizon_span * white_hotspot;

        // Cool light sits above the white core and warm light below it.
        // Both masks reach zero at the core so their colours blend through
        // the narrow hotspot without a hard horizontal boundary.
        let blue_above = smooth(-0.22, -0.12, centre) * (1.0 - smooth(-0.025, 0.005, centre));
        let amber_below = smooth(-0.005, 0.025, centre) * (1.0 - smooth(0.24, 0.46, centre));
        let blue_reflection = diffuse(centre + 0.055, 0.075) * horizon_span * blue_above;
        let amber = diffuse(centre - 0.060, 0.075) * horizon_span * amber_below;
        // Refracted light disperses through the whole lower bowl. Its density
        // falls towards the bottom pole and outer wall instead of ending as a
        // bright stripe suspended over black.
        let lower_bowl = smooth(-0.06, 0.38, centre)
            * (1.0 - smooth(0.42, 0.96, ny))
            * (1.0 - smooth(0.82, 1.0, radius));
        let lower_warmth = 1.0 - smooth(0.10, 0.58, centre);

        // Razor-thin inset arcs retain the wet specular edge without becoming
        // vector strokes. Their widths are 70% below the previous arcs, and
        // both still vanish completely at the middle sides (#3050).
        let fresnel = edge.powf(3.8);
        let top_pole = smooth(0.03, 0.88, -ny);
        let bottom_pole = smooth(0.03, 0.88, ny);
        let pole_light = (top_pole + bottom_pole).clamp(0.0, 1.0);
        let top_left = 0.90 + 0.10 * smooth(0.0, 0.80, -nx);
        let shimmer = 0.985 + 0.015 * sine(ny * 2.0 + t * 0.30);
        let top_arc = bell(radius - 0.988, 0.014) * top_pole * top_left * shimmer;
        let bottom_arc = bell(radius - 0.988, 0.012) * bottom_pole * shimmer;
        let chromatic = fresnel * pole_light;
        let warm_edge = (ny * 0.5 + 0.5).clamp(0.0, 1.0);

        // A broader Fresnel halo sits inside the razor-thin specular edge.
        // Warm upper and cool lower walls overlap smoothly around the sides,
        // giving the clear centre a continuous, volumetric glass boundary.
        let inner_wall = diffuse(radius - 0.89, 0.10) * (1.0 - smooth(0.94, 1.0, radius));
        let warm_wall = inner_wall * (1.0 - smooth(-0.18, 0.22, ny));
        let cool_wall = inner_wall * smooth(-0.22, 0.18, ny);
        // Separate side bevels bridge the pole highlights into one continuous
        // wall. Amber stays above the equator and silver-blue wraps below it.
        let side_wall = diffuse(radius - 0.915, 0.065) * smooth(0.15, 0.78, side);
        let warm_side = side_wall * (1.0 - smooth(-0.35, 0.15, ny));
        let cool_side = side_wall * smooth(-0.15, 0.35, ny);

        // Dodge-and-burn the glass wall without moving the horizon. A deep
        // channel immediately inside the extreme specular edge separates it
        // from the softer inset crescents, creating spherical wall thickness.
        let burn_channel = diffuse(radius - 0.92, 0.040);
        let upper_crescent =
            diffuse(radius - 0.81, 0.075) * smooth(-0.02, 0.72, -ny) * smooth(0.12, 0.80, -ny);
        let lower_crescent =
            diffuse(radius - 0.81, 0.075) * smooth(-0.02, 0.72, ny) * smooth(0.12, 0.80, ny);

        // A soft inner vignette and brighter centre suggest lens depth without
        // blurring the material or introducing a backdrop GPUI cannot sample.
        let absorption = nz.powf(1.30);
        let inner_shadow = smooth(0.30, 0.99, radius) * (0.68 - pole_light * 0.18);
        let inner_vignette = smooth(0.48, 0.99, radius) * (1.0 - pole_light) * 0.22;
        let body_shade =
            (1.0 - inner_shadow * 0.60 - inner_vignette - burn_channel * 0.48).max(0.12);
        let vertical = 0.5 - ny * 0.5;
        let light_field = (0.31 + absorption * 0.52 + vertical * 0.20) * body_shade;
        let body = 0.004 + light_field * 0.022;
        let sheen = bell(flow_x + 0.18, 0.62)
            * bell(flow_y + 0.47, 0.28)
            * (0.12 + absorption * 0.10)
            * envelope;

        let red = body * 0.50
            + horizon * 0.20 * body_shade
            + white * 2.25
            + amber * 1.42 * body_shade
            + blue_reflection * 0.025
            + lower_bowl * (0.08 + lower_warmth * 0.18)
            + warm_wall * 0.38
            + cool_wall * 0.025
            + warm_side * 0.46
            + cool_side * 0.08
            + upper_crescent * 0.20
            + lower_crescent * 0.17
            + sheen * 0.20
            + top_arc * 1.70
            + bottom_arc * 0.72
            + chromatic * (0.018 + warm_edge * 0.052);
        let green = body * 0.72
            + horizon * 0.25 * body_shade
            + white * 2.18
            + amber * 0.52 * body_shade
            + blue_reflection * 0.52
            + lower_bowl * (0.12 + lower_warmth * 0.04)
            + warm_wall * 0.16
            + cool_wall * 0.18
            + warm_side * 0.22
            + cool_side * 0.28
            + upper_crescent * 0.25
            + lower_crescent * 0.09
            + sheen * 0.26
            + top_arc * 1.82
            + bottom_arc * 1.08
            + chromatic * 0.022;
        let blue = body * 0.96
            + horizon * 0.34 * body_shade
            + white * 2.08
            + amber * 0.035 * body_shade
            + blue_reflection * 1.42
            + lower_bowl * (0.22 - lower_warmth * 0.17)
            + warm_wall * 0.055
            + cool_wall * 0.42
            + warm_side * 0.06
            + cool_side * 0.55
            + upper_crescent * 0.34
            + lower_crescent * 0.018
            + sheen * 0.34
            + top_arc * 2.00
            + bottom_arc * 1.30
            + chromatic * (0.018 + (1.0 - warm_edge) * 0.058);
        // A faint slate body gives the lens a physical volume while keeping
        // the page behind it clearly visible through the centre.
        let alpha = (0.055
            + absorption * 0.025
            + edge * 0.13
            + top_arc * 0.70
            + bottom_arc * 0.48
            + horizon * 0.17
            + white * 0.64
            + amber * 0.25
            + blue_reflection * 0.22
            + lower_bowl * 0.045
            + inner_wall * 0.055
            + side_wall * 0.070
            + (upper_crescent + lower_crescent) * 0.075
            + sheen * 0.025)
            .clamp(0.0, 0.96)
            * coverage;
        *pixel = bgra(red, green, blue, alpha);
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

/// Long, soft optical falloff without a per-pixel exponential call. Unlike a
/// compact bell, the tail has no hard boundary that could read as a stroke.
fn diffuse(value: f32, width: f32) -> f32 {
    let distance = value / width;
    let falloff = 1.0 / (1.0 + distance * distance);
    falloff * falloff
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

    #[test]
    fn the_orb_is_high_resolution_and_actually_transparent() {
        assert_eq!(ORB_PIXELS, 1024);
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
        assert!(bytes[centre + 3] > 20 && bytes[centre + 3] < 230);
    }

    #[test]
    fn the_orb_moves_and_answers_the_microphone() {
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
        let visibly_changed = before
            .iter()
            .zip(after)
            .filter(|(a, b)| (**a).abs_diff(**b) > 8)
            .count();
        assert!(
            visibly_changed > before.len() / 200,
            "motion must reshape visible pixels, not merely change one sample"
        );
        let mut spoken = input;
        for (index, level) in spoken.levels.iter_mut().enumerate() {
            *level = if index % 2 == 0 { 0.05 } else { 0.95 };
        }
        let louder = render(&surface, 0.2, spoken, 0.9);
        let speech_changed = before
            .iter()
            .zip(louder.as_bytes(0).expect("louder frame"))
            .filter(|(a, b)| (**a).abs_diff(**b) > 8)
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
    fn the_soft_rim_is_brightest_at_the_top_and_bottom() {
        let surface = Surface::new(96);
        let frame = render(
            &surface,
            0.2,
            OrbInput {
                recording: true,
                levels: [0.2; VOICE_SAMPLES],
                colour: 0xffbf47,
            },
            0.2,
        );
        let bytes = frame.as_bytes(0).expect("one frame");
        let rim_light = |nx: f32, ny: f32| {
            let ring = ORB_RADIUS * 0.988;
            let column = (((nx * ring + 1.0) * 0.5 * 96.0) as usize).min(95);
            let row = (((ny * ring + 1.0) * 0.5 * 96.0) as usize).min(95);
            let index = (row * 96 + column) * 4;
            bytes[index] as u16 + bytes[index + 1] as u16 + bytes[index + 2] as u16
        };
        let top = rim_light(0.0, -1.0);
        let bottom = rim_light(0.0, 1.0);
        let left = rim_light(-1.0, 0.0);
        let right = rim_light(1.0, 0.0);
        assert!(
            top > left + 80 && bottom > right + 80,
            "the poles must glow while the side rim fades instead of forming a white stroke"
        );
    }

    #[test]
    fn sine_wraps_when_rounding_reaches_the_table_end() {
        let almost_one_turn = f32::from_bits(0x3f7f_ffff);
        assert_eq!((almost_one_turn * SINE_STEPS as f32) as usize, SINE_STEPS);
        assert!(sine(TAU * almost_one_turn).abs() < 0.001);
    }
}
