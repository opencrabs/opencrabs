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

#[derive(Clone, Copy)]
struct OrbInput {
    recording: bool,
    level: f32,
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
            level: 0.0,
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
                    let target = if input.recording { input.level } else { 0.0 };
                    let rate = if target > energy { 0.42 } else { 0.12 };
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

    pub(crate) fn update(&self, recording: bool, level: f32, colour: u32) {
        *lock(&self.input) = OrbInput {
            recording,
            level: level.clamp(0.0, 1.0),
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

/// Screen-space measurements shared by every animation frame.
struct Surface {
    size: u32,
    x: Vec<f32>,
    y: Vec<f32>,
    radius: Vec<f32>,
}

impl Surface {
    fn new(size: u32) -> Self {
        let count = (size * size) as usize;
        let mut surface = Self {
            size,
            x: vec![0.0; count],
            y: vec![0.0; count],
            radius: vec![0.0; count],
        };
        for row in 0..size {
            let y = ((row as f32 + 0.5) / size as f32) * 2.0 - 1.0;
            for column in 0..size {
                let x = ((column as f32 + 0.5) / size as f32) * 2.0 - 1.0;
                let index = row as usize * size as usize + column as usize;
                surface.x[index] = x;
                surface.y[index] = y;
                surface.radius[index] = (x * x + y * y).sqrt() / ORB_RADIUS;
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
    let speed = if input.recording { 1.0 } else { 0.38 };
    let t = time * speed;
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
            let shadow = bell(radius - 1.02, 0.08)
                * (0.10 + 0.06 * ((y + 1.0) * 0.5).clamp(0.0, 1.0));
            let glow = bell(radius - 1.0, 0.10) * (0.025 + level * 0.04);
            *pixel = bgra(
                tint.0 * 0.025,
                tint.1 * 0.03,
                tint.2 * 0.045,
                shadow + glow,
            );
            continue;
        }

        let coverage = ((1.0 - radius) * surface.size as f32 * ORB_RADIUS * 0.5 + 0.5)
            .clamp(0.0, 1.0);
        let nx = x / ORB_RADIUS;
        let ny = y / ORB_RADIUS;
        let nz = (1.0 - radius * radius).max(0.0).sqrt();
        let edge = 1.0 - nz;

        // Bend the internal light field along the sphere normal. This gives
        // the transparent material a distorted, refracted appearance without
        // pretending that GPUI has sampled the pixels behind the window.
        let lens = 0.14 + edge * 0.24;
        let turbulence = 0.010 + level * 0.032;
        let flow_x = x - nx * lens + sine(y * 4.4 + t * 0.50) * turbulence;
        let flow_y = y - ny * lens
            + sine(x * 3.6 - t * 0.65) * turbulence
            + sine(x * 8.2 + y * 2.1 + t) * turbulence * 0.35;
        let side = flow_x.abs().clamp(0.0, 1.0);

        // Siri's main reflection is a curved V, not a horizontal white bar.
        // Copper collects at its centre and silver-blue wraps around both
        // sides. A narrow inner band gives the centre its bright point light.
        let centre_line = 0.10 - 0.26 * side.powf(1.35)
            + sine(flow_x * 3.3 - t * 0.72) * (0.018 + level * 0.035)
            + sine(flow_x * 7.7 + t * 1.10) * (0.006 + level * 0.012);
        let centre = flow_y - centre_line;
        let belt = bell(centre, 0.050 + (1.0 - side) * 0.050 + level * 0.025);
        let sharp = bell(centre, 0.018 + level * 0.012);
        let warm = bell(flow_x, 0.44);
        let band_r = 0.30 * (1.0 - warm) + warm;
        let band_g = 0.62 * (1.0 - warm) + 0.55 * warm;
        let band_b = 0.95 * (1.0 - warm) + 0.25 * warm;
        let envelope = 1.0 - smooth(0.78, 0.99, radius) * 0.88;
        let band = belt * (0.66 + level * 0.34) * envelope;
        let hot = sharp * warm * envelope;
        let centre_light = bell(flow_x, 0.26) * bell(flow_y - 0.08, 0.20);

        // Fainter internal reflections cross at another depth. Their paths
        // tighten near the lower rim, like light trapped inside a water drop.
        let upper_line = -0.34 - 0.15 * side.powf(1.6)
            + sine(flow_x * 4.1 + t * 0.43) * (0.012 + level * 0.018);
        let upper = bell(flow_y - upper_line, 0.018 + level * 0.010)
            * (0.30 + side * 0.70)
            * envelope;
        let lower_line = 0.43
            + 0.20 * side.powf(1.4)
            + sine(flow_x * 5.4 - t * 0.55) * (0.018 + level * 0.024);
        let lower_a = bell(flow_y - lower_line, 0.024 + level * 0.010);
        let lower_b = bell(
            flow_y - 0.57 - 0.12 * side
                + sine(flow_x * 4.6 + t * 0.47) * (0.015 + level * 0.018),
            0.014 + level * 0.007,
        );
        let caustics = (lower_a * 0.58 + lower_b * 0.36) * envelope;

        // Bright, selective edge lights and dark intervals make a hard surface.
        // A uniform white ring would flatten it back into an outlined circle.
        let top_left = (-nx * 0.55 - ny * 0.85).clamp(0.0, 1.0);
        let bottom_right = (nx * 0.45 + ny * 0.85).clamp(0.0, 1.0);
        let outer_rim = bell(radius - 0.976, 0.024);
        let inner_rim = bell(radius - 0.885, 0.055);
        let rim_base = outer_rim * (0.16 + top_left * 0.40);
        let rim_cool = outer_rim * top_left * 0.72 + inner_rim * top_left * 0.22;
        let rim_warm = inner_rim * (0.08 + bottom_right * 0.62);
        let specular = bell(nx + 0.40, 0.22) * bell(ny + 0.48, 0.12)
            + bell(nx + 0.58, 0.10) * bell(ny + 0.20, 0.28) * 0.38;
        let inner_shadow = smooth(0.45, 0.98, radius) * (1.0 - top_left * 0.35);
        let body_shade = 1.0 - inner_shadow * 0.52;
        let wisps = (0.5 + 0.5 * sine(flow_x * 3.4 + flow_y * 4.6 + t * 0.35))
            * (0.5 + 0.5 * sine(flow_y * 5.8 - flow_x * 2.2 - t * 0.28));
        let body = (0.004 + nz * 0.009 + wisps * 0.005) * body_shade;

        let red = body * 0.80
            + band * band_r * body_shade
            + hot * 0.72
            + centre_light * 0.30
            + upper * (0.20 + warm * 0.30)
            + caustics * (0.22 + warm * 0.52)
            + rim_base * 0.65
            + rim_cool * 0.38
            + rim_warm * 0.82
            + specular * 0.95
            + edge * edge * tint.0 * 0.025;
        let green = body
            + band * band_g * body_shade
            + hot * 0.62
            + centre_light * 0.25
            + upper * (0.34 + warm * 0.14)
            + caustics * (0.36 + warm * 0.23)
            + rim_base * 0.72
            + rim_cool * 0.66
            + rim_warm * 0.43
            + specular * 0.97
            + edge * edge * tint.1 * 0.025;
        let blue = body * 1.35
            + band * band_b * body_shade
            + hot * 0.40
            + centre_light * 0.17
            + upper * 0.56
            + caustics * (0.62 - warm * 0.25)
            + rim_base * 0.82
            + rim_cool
            + rim_warm * 0.20
            + specular
            + edge * edge * tint.2 * 0.035;
        let alpha = (0.28
            + edge * 0.50
            + band * 0.20
            + hot * 0.10
            + outer_rim * 0.18
            + specular * 0.08)
            .clamp(0.0, 0.96)
            * coverage;
        *pixel = bgra(red, green, blue, alpha);
    }
    RenderImage::new(vec![Frame::new(pixels)])
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
                level: 0.4,
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
            level: 0.2,
            colour: 0xffbf47,
        };
        let first = render(&surface, 0.2, input, 0.2);
        let later = render(&surface, 0.8, input, 0.2);
        assert_ne!(first.as_bytes(0), later.as_bytes(0));
        let louder = render(&surface, 0.2, input, 0.9);
        assert_ne!(first.as_bytes(0), louder.as_bytes(0));
    }

    #[test]
    fn sine_wraps_when_rounding_reaches_the_table_end() {
        let almost_one_turn = f32::from_bits(0x3f7f_ffff);
        assert_eq!(
            (almost_one_turn * SINE_STEPS as f32) as usize,
            SINE_STEPS
        );
        assert!(sine(TAU * almost_one_turn).abs() < 0.001);
    }
}
