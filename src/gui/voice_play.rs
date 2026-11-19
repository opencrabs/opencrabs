//! Seekable playback for the window's voice messages (#3044, #3054).
//!
//! One clip is active at a time. Its output stream and sink stay on the
//! playback thread, while the UI sends pause and seek commands through a
//! channel and reads lock-protected snapshots on the normal dashboard tick.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, Sink};

use crate::logging;
use crate::voice_media;

const POLL: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClipState {
    pub(crate) position: Duration,
    pub(crate) duration: Duration,
    pub(crate) playing: bool,
}

/// Everything the window compares to decide whether voice UI needs a paint.
#[derive(Default)]
pub(crate) struct PaintState {
    playback: Option<(PathBuf, ClipState)>,
    pending_visuals: usize,
    visual_generation: u64,
}

impl PartialEq for PaintState {
    fn eq(&self, other: &Self) -> bool {
        self.playback == other.playback
            && self.pending_visuals == other.pending_visuals
            && self.visual_generation == other.visual_generation
    }
}

impl Eq for PaintState {}

enum Command {
    Toggle,
    Seek(f32),
    Stop,
}

struct Active {
    id: u64,
    path: PathBuf,
    commands: Sender<Command>,
    state: Arc<Mutex<ClipState>>,
}

fn slot() -> &'static Mutex<Option<Active>> {
    static SLOT: Mutex<Option<Active>> = Mutex::new(None);
    &SLOT
}

fn next_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The playback position and paused state for one visible preview.
pub(crate) fn state(path: &Path) -> Option<ClipState> {
    let active = match slot().lock() {
        Ok(active) => active,
        Err(poisoned) => poisoned.into_inner(),
    };
    let active = active.as_ref().filter(|active| active.path == path)?;
    let state = match active.state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    Some(*state)
}

/// A single snapshot for the dashboard poll. Position changes repaint active
/// playback, and the visual count repaints a waveform when its decode ends.
pub(crate) fn paint_state() -> PaintState {
    let playback = {
        let active = match slot().lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.as_ref().map(|active| {
            let state = match active.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            (active.path.clone(), *state)
        })
    };
    PaintState {
        playback,
        pending_visuals: voice_media::pending(),
        visual_generation: voice_media::generation(),
    }
}

/// Start a clip, or pause/resume it when its own button is pressed again.
pub(crate) fn toggle(path: &Path) -> Result<(), String> {
    if send_existing(path, Command::Toggle) {
        return Ok(());
    }
    start(path, None)
}

/// Seek to a fraction of the clip. Seeking an inactive clip starts it there;
/// seeking a paused clip changes its position without unpausing it.
pub(crate) fn seek(path: &Path, fraction: f32) -> Result<(), String> {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if send_existing(path, Command::Seek(fraction)) {
        return Ok(());
    }
    start(path, Some(fraction))
}

/// True when the command went to this clip. A disconnected worker has already
/// finished, so its stale slot is removed and the caller starts a fresh one.
fn send_existing(path: &Path, command: Command) -> bool {
    let mut active = match slot().lock() {
        Ok(active) => active,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(current) = active.as_ref().filter(|active| active.path == path) else {
        return false;
    };
    let id = current.id;
    match current.commands.send(command) {
        Ok(()) => true,
        Err(_) => {
            if active.as_ref().is_some_and(|active| active.id == id) {
                *active = None;
            }
            false
        }
    }
}

fn start(path: &Path, initial_seek: Option<f32>) -> Result<(), String> {
    let previous = {
        let mut active = match slot().lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.take()
    };
    if let Some(previous) = previous {
        stop_worker(previous);
    }

    let (commands, receiver) = channel();
    let id = next_id();
    let state = Arc::new(Mutex::new(ClipState::default()));
    {
        let mut active = match slot().lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        *active = Some(Active {
            id,
            path: path.to_path_buf(),
            commands,
            state: Arc::clone(&state),
        });
    }
    let owned = path.to_path_buf();
    let worker_state = Arc::clone(&state);
    if let Err(error) = std::thread::Builder::new()
        .name("gui-voice-play".into())
        .spawn(move || run(owned, id, receiver, worker_state, initial_seek))
    {
        let mut active = match slot().lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        if active.as_ref().is_some_and(|active| active.id == id) {
            *active = None;
        }
        return Err(format!("the playback thread did not start: {error}"));
    }
    Ok(())
}

/// Stop one preview if it is active. Removing a composer attachment uses this
/// instead of toggle, because toggle would pause the state rather than end it.
pub(crate) fn stop(path: &Path) {
    let previous = {
        let mut active = match slot().lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        if active.as_ref().is_some_and(|active| active.path == path) {
            active.take()
        } else {
            None
        }
    };
    if let Some(previous) = previous {
        stop_worker(previous);
    }
}

fn stop_worker(active: Active) {
    if active.commands.send(Command::Stop).is_err() {
        logging::debug("voice playback: the previous worker had already stopped");
    }
}

fn run(
    path: PathBuf,
    id: u64,
    commands: Receiver<Command>,
    state: Arc<Mutex<ClipState>>,
    initial_seek: Option<f32>,
) {
    if let Err(reason) = play(&path, &commands, &state, initial_seek) {
        logging::error(&format!("voice playback: {reason}"));
    }
    let mut active = match slot().lock() {
        Ok(active) => active,
        Err(poisoned) => poisoned.into_inner(),
    };
    if active.as_ref().is_some_and(|active| active.id == id) {
        *active = None;
    }
}

fn play(
    path: &Path,
    commands: &Receiver<Command>,
    state: &Arc<Mutex<ClipState>>,
    initial_seek: Option<f32>,
) -> Result<(), String> {
    let (samples, rate) = voice_media::decode(path)?;
    if samples.is_empty() || rate == 0 {
        return Err(format!("{} holds no audio", path.display()));
    }
    let duration = Duration::from_secs_f64(samples.len() as f64 / f64::from(rate));
    let (_stream, handle) = OutputStream::try_default()
        .map_err(|error| format!("no audio output answered: {error}"))?;
    let sink = Sink::try_new(&handle)
        .map_err(|error| format!("the audio output would not start: {error}"))?;
    sink.append(SamplesBuffer::new(1, rate, samples));
    if let Some(fraction) = initial_seek {
        seek_sink(&sink, duration, fraction)?;
    }
    publish(state, &sink, duration);
    while !sink.empty() {
        let mut stopped = false;
        for command in commands.try_iter() {
            match command {
                Command::Toggle => {
                    if sink.is_paused() {
                        sink.play();
                    } else {
                        sink.pause();
                    }
                }
                Command::Seek(fraction) => seek_sink(&sink, duration, fraction)?,
                Command::Stop => {
                    sink.stop();
                    stopped = true;
                }
            }
        }
        publish(state, &sink, duration);
        if stopped {
            break;
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

fn seek_sink(sink: &Sink, duration: Duration, fraction: f32) -> Result<(), String> {
    sink.try_seek(duration.mul_f32(fraction))
        .map_err(|error| format!("the clip could not seek: {error}"))
}

fn publish(state: &Arc<Mutex<ClipState>>, sink: &Sink, duration: Duration) {
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    };
    *state = ClipState {
        position: sink.get_pos().min(duration),
        duration,
        playing: !sink.is_paused(),
    };
}
