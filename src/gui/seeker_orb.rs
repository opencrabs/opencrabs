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
        let first = render(&surface, 0.0, *lock(&input), 0.0);
        let latest = Arc::new(Mutex::new(first));
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
                    let next = render(&surface, started.elapsed().as_secs_f32(), input, energy);
                    *lock(&worker_latest) = Arc::new(next);
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
fn render(surface: &Surface, time: f32, input: OrbInput, energy: f32) -> Arc<RenderImage> {
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
            // A little coloured bloom lives outside the glass, in the texture's
            // transparent margin rather than in a second GPUI element.
            let glow = bell(radius - 1.0, 0.10) * (0.08 + level * 0.08);
            *pixel = bgra(tint.0 * 0.5, tint.1 * 0.5, tint.2 * 0.5, glow);
            continue;
        }

        let coverage = ((1.0 - radius) * surface.size as f32 * ORB_RADIUS * 0.5 + 0.5)
            .clamp(0.0, 1.0);
        let wave_a = sine(x * 4.2 + t * 1.45);
        let wave_b = sine(x * 8.1 - y * 2.4 - t * 0.82);
        let wave_c = sine(y * 5.3 - x * 2.1 + t * 0.63);
        let liquid = 0.018 + level * 0.065;
        let flow_x = x + wave_c * 0.025;
        let flow_y = y + wave_a * liquid + wave_b * (0.012 + level * 0.022);

        // Three ribbons cross at different depths. Their width and distortion
        // answer the microphone while their phases never stop moving.
        let centre = flow_y - sine(x * 2.65 - t * 0.72) * (0.035 + level * 0.06);
        let core = bell(centre, 0.085 + level * 0.065);
        let upper = bell(
            flow_y + 0.23 + sine(x * 3.1 + t * 0.55) * 0.055,
            0.032 + level * 0.025,
        );
        let lower = bell(
            flow_y - 0.27 + sine(x * 3.8 - t * 0.48) * 0.045,
            0.045 + level * 0.025,
        );
        let wisps = (0.5 + 0.5 * sine(x * 3.4 + y * 4.6 + wave_b + t * 0.35))
            * (0.5 + 0.5 * sine(y * 5.8 - x * 2.2 - t * 0.28));

        let fresnel = smooth(0.42, 1.0, radius);
        let rim = bell(radius - 0.965, 0.075);
        let top = (-y).clamp(0.0, 1.0);
        let specular = bell(flow_x + 0.34 + sine(t * 0.4) * 0.07, 0.42)
            * bell(y + 0.56, 0.24)
            * 0.32;
        let horizontal = (1.0 - x * x * 0.48).clamp(0.0, 1.0);
        let band = core * horizontal * (0.82 + sine(t * 1.1) * 0.08 + level * 0.22);
        let white = (1.0 - x.abs() * 0.72).clamp(0.0, 1.0);
        let band_rgb = (
            tint.0 * (1.0 - white) + white,
            tint.1 * (1.0 - white) + white,
            tint.2 * (1.0 - white) + white,
        );
        let rim_light = rim * (0.12 + top * 0.48 + specular);

        let red = 0.012
            + wisps * 0.018
            + fresnel * tint.0 * 0.035
            + band * band_rgb.0 * 0.82
            + upper * tint.0 * 0.16
            + lower * tint.0 * 0.11
            + rim_light * 0.72
            + specular * 0.35;
        let green = 0.016
            + wisps * 0.022
            + fresnel * tint.1 * 0.035
            + band * band_rgb.1 * 0.86
            + upper * tint.1 * 0.18
            + lower * tint.1 * 0.12
            + rim_light * 0.78
            + specular * 0.38;
        let blue = 0.027
            + wisps * 0.034
            + fresnel * tint.2 * 0.045
            + band * band_rgb.2 * 0.92
            + upper * tint.2 * 0.22
            + lower * tint.2 * 0.16
            + rim_light * 0.88
            + specular * 0.42;
        let alpha = (0.16 + fresnel * 0.35 + band * 0.30 + rim * 0.13 + specular * 0.10)
            .clamp(0.0, 0.82)
            * coverage;
        *pixel = bgra(red, green, blue, alpha);
    }
    Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
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
    table[index] * (1.0 - fraction) + table[index + 1] * fraction
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
}
