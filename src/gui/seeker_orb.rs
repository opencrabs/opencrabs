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
                let started = Instant::now();
                let mut energy = 0.0_f32;
                while !worker_stop.load(Ordering::Relaxed) {
                    let began = Instant::now();
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
                    let next = Arc::new(render(
                        &surface,
                        started.elapsed().as_secs_f32(),
                        input,
                        energy,
                    ));
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
fn render(surface: &Surface, time: f32, input: OrbInput, energy: f32) -> RenderImage {
    let mut pixels = RgbaImage::new(surface.size, surface.size);
    let tint = rgb(input.colour);
    let level = energy.clamp(0.0, 1.0);
    let speed = if input.recording { 1.65 } else { 0.45 };
    let t = time * speed;
    // These are frame-wide uniforms, not per-pixel waves: the whole body of
    // liquid visibly rolls and breathes even when the room is quiet.
    let roll = (t * 0.32).sin() * (0.18 + level * 0.08);
    let (roll_sin, roll_cos) = roll.sin_cos();
    let sway = (t * 0.72).sin();
    let breathe = 0.5 + 0.5 * (t * 0.90).sin();
    for (index, pixel) in pixels.pixels_mut().enumerate() {
        let radius = surface.radius[index];
        if radius > 1.10 {
            continue;
        }
        let x = surface.x[index];
        let y = surface.y[index];
        if radius > 1.0 {
            // The transparent margin carries a soft contact shadow and a
            // faint light bloom. The sphere now sits in the window instead of
            // reading as a circular filter laid over the text.
            let shadow =
                bell(radius - 1.02, 0.08) * (0.10 + 0.06 * ((y + 1.0) * 0.5).clamp(0.0, 1.0));
            let glow = bell(radius - 1.0, 0.10) * (0.025 + level * 0.04);
            *pixel = bgra(tint.0 * 0.025, tint.1 * 0.03, tint.2 * 0.045, shadow + glow);
            continue;
        }

        let coverage =
            ((1.0 - radius) * surface.size as f32 * ORB_RADIUS * 0.5 + 0.5).clamp(0.0, 1.0);
        let nx = x / ORB_RADIUS;
        let ny = y / ORB_RADIUS;
        let nz = surface.normal_z[index];
        let edge = 1.0 - nz;

        // Refract the internal volume along the sphere normal, then push two
        // crossing liquid waves through it. The old offsets were too small to
        // read as motion; these deform the bands themselves, not just tint.
        let lens = 0.16 + edge * 0.30;
        let turbulence = 0.045 + level * 0.16;
        let liquid_x = nx * roll_cos - ny * roll_sin;
        let liquid_y = nx * roll_sin + ny * roll_cos;
        let voice = voice_at(&input.levels, (liquid_x * 0.5 + 0.5).clamp(0.0, 1.0));
        let voice_envelope = (1.0 - nx * nx).max(0.0).sqrt();
        let voice_ripple = voice * voice_envelope;
        let wave_x =
            sine(liquid_y * 5.0 + t * 1.35) + sine((liquid_x + liquid_y) * 7.0 - t * 0.80) * 0.45;
        let wave_y = sine(liquid_x * 6.2 - t * 1.05) + sine((liquid_x - liquid_y) * 8.0 + t) * 0.40;
        // The meter is spatial here, not just a brightness uniform: every
        // horizontal slice follows its own recent peak, so the liquid ribbon
        // bends into the same scrolling wave the old meter displayed.
        let voice_wave = sine(liquid_x * 5.5 - t * 2.2) * voice_ripple;
        let flow_x = x - nx * lens
            + wave_x * turbulence
            + sine(liquid_y * 4.0 + t * 1.7) * voice_ripple * 0.06;
        let flow_y = y - ny * lens + wave_y * turbulence + voice_wave * 0.22;
        let side = flow_x.abs().clamp(0.0, 1.0);

        // A broad, white-hot core sits inside a blue and rose-gold ribbon.
        // Its curvature and position evolve with the liquid, so the highlight
        // belongs to the volume instead of looking painted on the window.
        let centre_line = 0.02
            + 0.20 * side.powf(1.45)
            + sway * 0.080
            + sine(flow_x * 3.3 - t * 1.20) * (0.035 + level * 0.045)
            + sine(flow_x * 7.7 + t * 1.60) * (0.012 + level * 0.018);
        let centre = flow_y - centre_line;
        let belt = bell(
            centre,
            0.075 + (1.0 - side) * 0.075 + level * 0.035 + breathe * 0.018,
        );
        let sharp = bell(centre, 0.030 + level * 0.016);
        let warm = bell(flow_x, 0.55);
        let band_r = 0.34 * (1.0 - warm) + warm;
        let band_g = 0.68 * (1.0 - warm) + 0.62 * warm;
        let band_b = (1.0 - warm) + 0.34 * warm;
        let envelope = 1.0 - smooth(0.82, 1.0, radius) * 0.82;
        let band = belt * (0.88 + level * 0.48) * envelope;
        let hot = sharp * warm * envelope;
        let centre_light = bell(flow_x, 0.38) * bell(centre, 0.24) * envelope;
        let core = bell(flow_x, 0.18) * bell(centre, 0.105) * envelope;

        // Secondary caustics cross at different depths and drift against the
        // main band. This parallax is what makes the material read as water.
        let upper_line = -0.34 - 0.15 * side.powf(1.6) - sway * 0.045
            + sine(flow_x * 4.1 + t * 0.95) * (0.030 + level * 0.028);
        let upper =
            bell(flow_y - upper_line, 0.032 + level * 0.014) * (0.38 + side * 0.62) * envelope;
        let lower_line = 0.43
            + 0.20 * side.powf(1.4)
            + sway * 0.055
            + sine(flow_x * 5.4 - t * 1.15) * (0.036 + level * 0.032);
        let lower_a = bell(flow_y - lower_line, 0.034 + level * 0.014);
        let lower_b = bell(
            flow_y - 0.57 - 0.12 * side + sine(flow_x * 4.6 + t * 1.05) * (0.030 + level * 0.026),
            0.024 + level * 0.010,
        );
        let caustics = (lower_a * 0.72 + lower_b * 0.48) * envelope;

        // Fresnel gives the rim depth; broad bilateral glints keep opposite
        // sides bright instead of leaving one white dot on a dark circle.
        let fresnel = edge.powf(2.4);
        let top_left = (-nx * 0.55 - ny * 0.85).clamp(0.0, 1.0);
        let bottom_right = (nx * 0.45 + ny * 0.85).clamp(0.0, 1.0);
        let opposite_sides = smooth(0.18, 0.92, nx.abs());
        let opposite_caps = smooth(0.30, 0.95, ny.abs());
        let shimmer = 0.76 + 0.24 * sine((nx + ny) * 3.0 + t * 1.10);
        let outer_rim = bell(radius - 0.955, 0.050);
        let inner_rim = bell(radius - 0.840, 0.100);
        let rim_base = outer_rim * (0.24 + opposite_sides * 0.76 + opposite_caps * 0.42) * shimmer;
        let rim_cool = outer_rim * (0.32 + opposite_sides * 0.82 + opposite_caps * 0.46)
            + inner_rim * (0.12 + top_left * 0.32 + opposite_sides * 0.28);
        let rim_warm = inner_rim * (0.16 + bottom_right * 0.42 + opposite_sides * 0.50)
            + outer_rim * opposite_caps * 0.20;
        let side_specular = bell(nx.abs() - 0.70, 0.24) * bell(ny + 0.05, 0.62);
        let cap_specular = bell(ny.abs() - 0.74, 0.18) * bell(nx, 0.55);
        let specular = side_specular * 0.78 + cap_specular * 0.48;
        let inner_shadow = smooth(0.38, 0.99, radius) * (0.72 - top_left * 0.25);
        let body_shade = 1.0 - inner_shadow * 0.58;
        let wisps = (0.5 + 0.5 * sine(flow_x * 3.4 + flow_y * 4.6 + t * 0.85))
            * (0.5 + 0.5 * sine(flow_y * 5.8 - flow_x * 2.2 - t * 0.72));
        let body = (0.006 + nz * 0.014 + wisps * 0.010) * body_shade;

        let red = body * 0.80
            + band * band_r * body_shade
            + hot * 0.86
            + core * 1.15
            + centre_light * 0.52
            + upper * (0.24 + warm * 0.34)
            + caustics * (0.28 + warm * 0.58)
            + rim_base * 0.72
            + rim_cool * 0.44
            + rim_warm * 0.88
            + specular
            + fresnel * tint.0 * 0.040;
        let green = body
            + band * band_g * body_shade
            + hot * 0.78
            + core * 1.08
            + centre_light * 0.48
            + upper * (0.40 + warm * 0.18)
            + caustics * (0.42 + warm * 0.26)
            + rim_base * 0.80
            + rim_cool * 0.74
            + rim_warm * 0.48
            + specular
            + fresnel * tint.1 * 0.045;
        let blue = body * 1.35
            + band * band_b * body_shade
            + hot * 0.64
            + core * 0.95
            + centre_light * 0.42
            + upper * 0.66
            + caustics * (0.72 - warm * 0.24)
            + rim_base * 0.90
            + rim_cool
            + rim_warm * 0.24
            + specular
            + fresnel * tint.2 * 0.060;
        let alpha = (0.16
            + edge * 0.42
            + fresnel * 0.10
            + band * 0.20
            + hot * 0.14
            + core * 0.16
            + outer_rim * 0.20
            + specular * 0.10)
            .clamp(0.0, 0.97)
            * coverage;
        *pixel = bgra(red, green, blue, alpha);
    }
    RenderImage::new(vec![Frame::new(pixels)])
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
    fn sine_wraps_when_rounding_reaches_the_table_end() {
        let almost_one_turn = f32::from_bits(0x3f7f_ffff);
        assert_eq!((almost_one_turn * SINE_STEPS as f32) as usize, SINE_STEPS);
        assert!(sine(TAU * almost_one_turn).abs() < 0.001);
    }
}
