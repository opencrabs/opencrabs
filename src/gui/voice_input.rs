//! Microphone capture for the composer, and the transcription that follows it.
//!
//! The tree can already turn audio into text (`channels::voice::transcribe`).
//! Nothing in it can produce audio from a local device: voice notes arrive as
//! files from a channel, so recording was never needed. A window with a
//! composer does need it. This module opens the default input device with
//! `cpal`, collects mono samples for as long as the recording runs, and hands
//! back one in-memory WAV: 16 kHz, mono, PCM16, which is what every speech
//! endpoint in the chain accepts and what whisper wants without a resample of
//! its own. That WAV then goes through the same chain a voice note goes
//! through.
//!
//! The stream never leaves the thread that opened it. `cpal::Stream` is `!Send`
//! on macOS and the backend keeps a pointer to the device it was built from, so
//! building it here and moving it anywhere else aborts the process. The thread
//! therefore owns the device, the stream and the sample buffer, and reports
//! through a channel; the window only sees ordinary messages.
//!
//! Both jobs run on their own threads and answer through channels, which the
//! window's 50 ms poll tick drains: no frame waits on a device or a network.
//! What leaves this module is a WAV, a string, or a line explaining what failed
//! (no device, no permission, a format we cannot read, a provider chain with
//! nothing in it). Nothing here writes config.
//!
//! A recording that comes back with words is saved as an OGG/Opus voice note
//! under the profile's `voice/` folder (#3044). That is the file the message
//! carries, the same artifact a channel would have delivered, so the row can
//! play back what was actually said instead of only the transcript. The save
//! happens after the transcription, not before: a recording nobody could
//! transcribe becomes no message, and a file for it would be an orphan.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use opencrabs::config::Config;

use crate::logging;

/// The rate every speech endpoint and the bundled whisper accept.
///
/// Devices open at whatever they prefer (44.1 or 48 kHz, usually); the
/// resample to this rate happens once, on the capture thread, before the WAV is
/// built. Sending 48 kHz and hoping the endpoint resamples costs upload size on
/// every recording and is refused outright by some local models.
pub(crate) const TARGET_RATE: u32 = 16_000;

/// Longest single recording, in seconds.
///
/// The buffer is capped rather than the clock, so a device that delivers
/// samples faster than real time cannot grow it without bound. Two minutes is
/// far past any spoken turn and keeps the worst case at ~8 MB of f32.
const MAX_SECONDS: u32 = 120;

/// Where a take with words is kept: `<profile home>/voice/`, beside `memory/`
/// and `logs/`. A profile's recordings belong to that profile.
pub(crate) const VOICE_DIR: &str = "voice";

/// What a recording is named, inside [`VOICE_DIR`].
///
/// The prefix is the contract, not decoration: a spoken reply is saved in the
/// same folder, and only a `voice-*` file is a message whose words came out of
/// the microphone. Without it the reply's own clip would read as a recording
/// and its marker would be taken back out of text the user actually typed.
const RECORDING_PREFIX: &str = "voice-";

/// What a spoken reply is named, inside [`VOICE_DIR`].
///
/// Deliberately not [`RECORDING_PREFIX`]: the reply is the agent's own voice,
/// and a turn that began with typing must never be mistaken for one the
/// microphone heard when the ledger is read back.
const REPLY_PREFIX: &str = "reply-";

/// How often the capture thread checks whether it should stop.
const TICK: Duration = Duration::from_millis(20);

/// How many meter readings the composer's waveform keeps.
///
/// One reading per tick, newest last, so the strip shows the last ~1.1 seconds
/// of the microphone: long enough to read as a wave and short enough that the
/// right-hand edge is what the person is saying right now. The reading is a
/// peak, not an RMS, because the point of the strip is "the microphone hears
/// you", and a peak moves on the first syllable instead of averaging it away.
pub(crate) const METER_BARS: usize = 56;

/// What the capture thread tells the window.
pub(crate) enum CaptureEvent {
    /// The device opened and the stream is delivering samples. The device and
    /// its rate are logged by the thread that read them: the window only needs
    /// to know the microphone is live, and a copy nobody reads is dead weight.
    Started,
    /// The recording ended. `Ok` carries the finished take.
    Finished(Result<Recording, String>),
}

/// One finished take: the audio, in the two shapes the next two steps want.
///
/// The WAV is what a speech endpoint is given, header included, so the
/// transcription thread never touches the samples. The PCM is the same audio
/// for the Opus encoder, which takes samples rather than a container. Both are
/// produced once, on the capture thread, from the same resample.
pub(crate) struct Recording {
    /// 16 kHz mono PCM16 WAV, ready to POST.
    pub(crate) wav: Vec<u8>,
    /// 16 kHz mono samples, ready to encode.
    pub(crate) pcm: Vec<i16>,
}

/// A finished recording with the words it carried (#3044).
///
/// `audio` is the saved voice note, and `None` means the save failed: the
/// transcript is still worth sending, so a missing clip never costs the user
/// their message. `seconds` is how long the take ran, measured from the samples
/// rather than the clock, so it is the length of the clip the user will hear.
pub(crate) struct Spoken {
    pub(crate) text: String,
    pub(crate) audio: Option<PathBuf>,
    pub(crate) seconds: f32,
}

/// One recording in flight.
///
/// Dropping it stops the microphone: the stop flag is the only thing the
/// capture thread watches, so an abandoned recording ends at the next tick
/// instead of holding the device for the life of the window.
pub(crate) struct Capture {
    stop: Arc<AtomicBool>,
    events: Receiver<CaptureEvent>,
    /// The rolling peak meter the composer's waveform draws, newest last. The
    /// capture thread appends one reading per tick and trims the front, so the
    /// window reads a snapshot instead of racing the device.
    levels: Arc<Mutex<Vec<f32>>>,
    started: Instant,
}

impl Capture {
    /// Open the default input device on its own thread and start recording.
    ///
    /// Returns immediately: opening a device blocks while the platform settles
    /// its audio session (and may put a permission prompt on screen on macOS),
    /// and the window cannot afford that on its UI thread. The outcome arrives
    /// as the first `CaptureEvent`.
    pub(crate) fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let levels: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, events) = channel();
        let flag = Arc::clone(&stop);
        let meter = Arc::clone(&levels);
        let spawned = std::thread::Builder::new()
            .name("gui-voice-capture".into())
            .spawn(move || run(tx, flag, meter));
        if let Err(e) = spawned {
            let (tx, rx) = channel();
            let _ = tx.send(CaptureEvent::Finished(Err(format!(
                "the microphone thread did not start: {e}"
            ))));
            return Self {
                stop,
                events: rx,
                levels,
                started: Instant::now(),
            };
        }
        Self {
            stop,
            events,
            levels,
            started: Instant::now(),
        }
    }

    /// Ask the capture thread to finish and build the WAV.
    ///
    /// Idempotent and callable from a click handler: the thread notices at the
    /// next tick, so the finished bytes arrive one or two polls later.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Take the next message, if there is one. Non-blocking.
    pub(crate) fn try_next(&self) -> Option<CaptureEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    /// How long the microphone has been held, for the composer's counter.
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The meter's current window, oldest first.
    ///
    /// A snapshot, not a handle: the composer paints whatever was true when the
    /// frame was built, and the device keeps writing underneath. An empty
    /// vector is a recording that has not delivered its first tick yet, which
    /// the waveform draws as a flat line rather than a gap.
    pub(crate) fn levels(&self) -> Vec<f32> {
        match self.levels.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // The thread holds its own clone; a dropped handle means nobody wants
        // the samples, so let the device go.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One transcription in flight.
///
/// The chain is `channels::voice::transcribe`, the function voice notes already
/// go through: box STT, an [OI]-compatible endpoint, Groq, then the bundled
/// whisper. Reading `Config::current()` here instead of a config captured at
/// boot is what makes the window inherit whatever the terminal and the channels
/// are set to, including an edit the reloader applied a moment ago.
pub(crate) struct Transcription {
    result: Receiver<Result<Spoken, String>>,
}

impl Transcription {
    /// Send a finished recording through the configured chain, off the UI
    /// thread, and save the voice note beside the words.
    ///
    /// Returns immediately. The UI keeps polling the same 50 ms tick it uses
    /// for the microphone, so the answer lands without a second timer.
    pub(crate) fn start(recording: Recording) -> Self {
        let (tx, result) = channel();
        let spawned = std::thread::Builder::new()
            .name("gui-voice-stt".into())
            .stack_size(STACK_BYTES)
            .spawn(move || {
                let _ = tx.send(transcribe(recording));
            });
        if let Err(e) = spawned {
            let (tx, result) = channel();
            let _ = tx.send(Err(format!("the transcription thread did not start: {e}")));
            return Self { result };
        }
        Self { result }
    }

    /// Take the answer if it has landed. Non-blocking.
    pub(crate) fn try_take(&self) -> Option<Result<Spoken, String>> {
        match self.result.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }
}

/// Transcribe one take, then keep its audio as a voice note.
///
/// The order matters: a take with no words becomes no message, so saving first
/// would leave a file nothing points at. A save that fails is logged and the
/// words still go out, because the transcript is what the agent answers.
fn transcribe(recording: Recording) -> Result<Spoken, String> {
    let seconds = recording.pcm.len() as f32 / TARGET_RATE as f32;
    let (text, language) = transcribe_wav(recording.wav)?;
    let audio = match save_voice_note(&recording.pcm) {
        Ok(path) => Some(path),
        Err(reason) => {
            logging::error(&format!("voice capture: {reason}"));
            None
        }
    };
    logging::info(&format!(
        "voice capture: {seconds:.1}s heard as {}",
        language.unwrap_or_else(|| "an unreported language".to_string()),
    ));
    Ok(Spoken {
        text,
        audio,
        seconds,
    })
}

/// Whether a path is one of the window's own recordings (#3044).
///
/// Only a recording is a message whose words came out of speech, so only its
/// marker is taken back out of the text the agent reads: the card in the row is
/// what carries the audio, and the agent gets the words it can answer. A clip
/// the user picked with the paperclip is a file the agent may need to open, and
/// that marker has to survive into the prompt.
pub(crate) fn is_recording(path: &Path) -> bool {
    let folder = opencrabs::config::opencrabs_home().join(VOICE_DIR);
    path.parent() == Some(folder.as_path())
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(RECORDING_PREFIX))
}

/// Write one take into the profile's `voice/` folder as an OGG/Opus file.
///
/// The same container the channels send, so the clip in the transcript is the
/// artifact a recipient of a voice note would have received. Named by the
/// millisecond it was saved: two recordings in one second would otherwise
/// overwrite each other, and the older message would start playing the newer
/// audio.
fn save_voice_note(pcm: &[i16]) -> Result<PathBuf, String> {
    let ogg = opencrabs::channels::voice::opus::encode_ogg_opus(pcm, TARGET_RATE)
        .map_err(|e| format!("the recording would not encode as a voice note: {e:#}"))?;
    let path = write_clip(&ogg, RECORDING_PREFIX)?;
    logging::info(&format!(
        "voice capture: saved {} bytes of ogg to {}",
        ogg.len(),
        path.display()
    ));
    Ok(path)
}

/// Keep one synthesized answer beside the recordings, for the turn that began
/// as one (#3044).
///
/// `synthesize` already returns OGG/Opus, so the bytes are written as they
/// arrive: re-encoding a finished clip would only cost time and quality. The
/// name does not start with [`RECORDING_PREFIX`], so a spoken reply is never
/// mistaken for something the microphone heard.
pub(crate) fn save_reply_audio(ogg: &[u8]) -> Result<PathBuf, String> {
    let path = write_clip(ogg, REPLY_PREFIX)?;
    logging::info(&format!(
        "voice reply: saved {} bytes of ogg to {}",
        ogg.len(),
        path.display()
    ));
    Ok(path)
}

/// Write one finished clip into the profile's `voice/` folder, named for the
/// millisecond it was saved.
///
/// Two recordings inside one second would otherwise overwrite each other, and
/// the older message would start playing the newer audio.
fn write_clip(bytes: &[u8], prefix: &str) -> Result<PathBuf, String> {
    let dir = opencrabs::config::opencrabs_home().join(VOICE_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("the voice folder would not open ({}): {e}", dir.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{prefix}{stamp}.ogg"));
    std::fs::write(&path, bytes)
        .map_err(|e| format!("the clip would not save ({}): {e}", path.display()))?;
    Ok(path)
}

fn transcribe_wav(wav: Vec<u8>) -> Result<(String, Option<String>), String> {
    let config = Config::current();
    let voice = config.voice_config();
    if !voice.stt_enabled {
        return Err(
            "no speech-to-text provider is configured yet: add one in Settings, and the microphone has somewhere to send the audio"
                .to_string(),
        );
    }

    // A runtime of its own: this is a capture thread with no reactor, and every
    // remote provider in the chain is a POST. `enable_all` covers the timers
    // the HTTP client arms for its own timeouts.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("the transcription runtime would not start: {e}"))?;
    let heard = runtime
        .block_on(
            async move { opencrabs::channels::voice::transcribe_with_language(wav, &voice).await },
        )
        .map_err(|e| format!("{e:#}"))?;
    let (text, language) = heard;

    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(
            "the transcription came back empty: check the microphone level and try again"
                .to_string(),
        );
    }
    logging::info(&format!(
        "voice capture: transcribed {} character(s) in {}",
        text.chars().count(),
        language.as_deref().unwrap_or("an unreported language"),
    ));
    Ok((text, language))
}

/// The stack the transcription thread runs on.
///
/// Same rule as every other off-thread config read in this window: a plain
/// `std::thread` default stack is thin enough that building a `Config`-shaped
/// value on it has already killed the process once (#2452).
const STACK_BYTES: usize = 4 * 1024 * 1024;

/// The capture thread's body: open, record, encode, report.
fn run(tx: Sender<CaptureEvent>, stop: Arc<AtomicBool>, meter: Arc<Mutex<Vec<f32>>>) {
    if let Err(reason) = record(&tx, &stop, &meter) {
        logging::error(&format!("voice capture: {reason}"));
        let _ = tx.send(CaptureEvent::Finished(Err(reason)));
    }
}

fn record(
    tx: &Sender<CaptureEvent>,
    stop: &Arc<AtomicBool>,
    meter: &Arc<Mutex<Vec<f32>>>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no microphone found; plug one in and try again".to_string())?;
    let name = device.name().unwrap_or_else(|_| "microphone".to_string());

    // The device's own default, not a guess at 16 kHz: asking for a rate the
    // device does not offer fails the build, and every macOS device offers its
    // own default and little else. The resample happens after capture.
    let supported = device
        .default_input_config()
        .map_err(|e| format!("the microphone would not say what it can record: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let rate = config.sample_rate.0;
    let channels = config.channels;

    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // The loudest sample since the last tick, as raw f32 bits. The callback
    // stores it and the tick below takes it, which is what turns a stream of
    // buffers of unknown size into a steady 50 readings a second: the waveform
    // scrolls at the same speed whatever the device's buffer size happens to
    // be.
    let peak = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let cap = (rate as usize)
        .saturating_mul(channels as usize)
        .saturating_mul(MAX_SECONDS as usize);

    let stream = build_stream(&device, &config, format, &buffer, &failure, &peak, cap)?;
    stream
        .play()
        .map_err(|e| format!("the microphone stream would not start: {e}"))?;

    let _ = tx.send(CaptureEvent::Started);

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(TICK);
        push_level(
            meter,
            f32::from_bits(peak.swap(0.0f32.to_bits(), Ordering::Relaxed)),
        );
    }

    // Drop the stream before reading the buffer, so no callback is mid-push
    // when the samples are taken.
    drop(stream);

    if let Ok(slot) = failure.lock()
        && let Some(reason) = slot.as_ref()
    {
        logging::error(&format!(
            "voice capture: the stream reported an error while recording: {reason}"
        ));
    }

    let raw = match buffer.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    let mono = to_mono(&raw, channels);
    if mono.is_empty() {
        return Err(
            "the microphone delivered no audio; check the system input device and its permission"
                .to_string(),
        );
    }
    let at_target = resample(&mono, rate, TARGET_RATE);
    let pcm = pcm16(&at_target);
    let wav = wav_bytes_from_pcm(&pcm, TARGET_RATE);
    logging::info(&format!(
        "voice capture: {} at {rate} Hz -> {:.1}s of 16 kHz mono, {} bytes",
        name,
        mono.len() as f32 / rate as f32,
        wav.len()
    ));
    let _ = tx.send(CaptureEvent::Finished(Ok(Recording { wav, pcm })));
    Ok(())
}

/// Build the input stream for whichever sample format the device speaks.
///
/// The callback is the only place that writes the buffer, and it writes f32 in
/// the device's own channel layout: the downmix to mono happens after the
/// stream stops, so the helpers below stay pure and testable.
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    buffer: &Arc<Mutex<Vec<f32>>>,
    failure: &Arc<Mutex<Option<String>>>,
    peak: &Arc<AtomicU32>,
    cap: usize,
) -> Result<cpal::Stream, String> {
    macro_rules! open {
        ($ty:ty, $convert:expr) => {{
            let sink = Arc::clone(buffer);
            let errors = Arc::clone(failure);
            let loudest = Arc::clone(peak);
            let convert: fn($ty) -> f32 = $convert;
            device
                .build_input_stream(
                    config,
                    move |data: &[$ty], _: &cpal::InputCallbackInfo| {
                        // The meter is a monotonic max over the tick, so it
                        // keeps up on the device's thread without a lock: two
                        // relaxed atomics per buffer, nothing the audio path
                        // can block on.
                        let here = data
                            .iter()
                            .fold(0.0f32, |loud, s| loud.max(convert(*s).abs()));
                        loudest.fetch_max(here.to_bits(), Ordering::Relaxed);
                        let mut guard = match sink.lock() {
                            Ok(guard) => guard,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        let room = cap.saturating_sub(guard.len());
                        guard.extend(data.iter().take(room).map(|s| convert(*s)));
                    },
                    move |err: cpal::StreamError| {
                        if let Ok(mut slot) = errors.lock() {
                            *slot = Some(err.to_string());
                        }
                    },
                    None,
                )
                .map_err(|e| format!("the microphone stream would not open: {e}"))
        }};
    }

    match format {
        SampleFormat::F32 => open!(f32, std::convert::identity),
        SampleFormat::F64 => open!(f64, |s| s as f32),
        SampleFormat::I16 => open!(i16, |s| s as f32 / 32_768.0),
        SampleFormat::U16 => open!(u16, |s| (s as f32 - 32_768.0) / 32_768.0),
        SampleFormat::I32 => open!(i32, |s| s as f32 / 2_147_483_648.0),
        SampleFormat::U8 => open!(u8, |s| (s as f32 - 128.0) / 128.0),
        other => Err(format!(
            "the microphone records in {other:?}, which this build cannot read"
        )),
    }
}

/// Append one reading to the meter, keeping its window fixed.
///
/// A `Vec` of 56 floats shifted by one is a memmove of 224 bytes on a thread
/// that sleeps 20 ms between pushes, so a deque's bookkeeping would cost more
/// than it saves. The lock is only ever held for that push: the composer takes
/// a clone and lets go.
fn push_level(meter: &Arc<Mutex<Vec<f32>>>, peak: f32) {
    let mut guard = match meter.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() >= METER_BARS {
        guard.remove(0);
    }
    // Full scale is 1.0 and the strip draws to the top of its box at 1.0; a
    // clipped input would otherwise paint past the row.
    guard.push(peak.clamp(0.0, 1.0));
}

/// Fold an interleaved buffer down to mono by averaging each frame.
///
/// Averaging rather than taking the first channel: a device that reports two
/// channels where only the left one carries the microphone would otherwise
/// record that one fine, but a device that puts the microphone on the right
/// would record silence.
pub(crate) fn to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let width = channels.max(1) as usize;
    if width == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(width)
        .map(|frame| frame.iter().sum::<f32>() / width as f32)
        .collect()
}

/// Resample to the rate speech endpoints expect, by linear interpolation.
///
/// Speech recognition is robust to a plain linear resample and this keeps the
/// dependency out of the capture path (`rubato` is only compiled in with
/// `local-stt`). Equal rates are a copy, not a pass through the loop.
pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 {
        return Vec::new();
    }
    if from == to {
        return input.to_vec();
    }
    let out_len = ((input.len() as u64 * to as u64) / from as u64) as usize;
    let step = from as f64 / to as f64;
    let last = input.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * step;
        let index = pos.floor() as usize;
        if index >= last {
            out.push(input[last]);
            continue;
        }
        let frac = (pos - index as f64) as f32;
        let a = input[index];
        let b = input[index + 1];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Encode mono f32 samples as a PCM16 WAV, header included.
///
/// Hand-built rather than through `hound`: hound is behind `local-stt`, and a
/// window built without that feature still has to be able to record. The
/// layout is the canonical 44-byte one and nothing here assumes a seekable
/// file, because the bytes go straight to an HTTP body.
pub(crate) fn wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    wav_bytes_from_pcm(&pcm16(samples), sample_rate)
}

/// Fold f32 samples to the PCM16 the WAV and the voice note both carry.
///
/// One conversion, shared by both: a clip whose WAV and whose saved voice note
/// were scaled differently would play back at two volumes depending on which
/// one you opened.
pub(crate) fn pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect()
}

/// The same WAV, from samples that are already folded.
pub(crate) fn wav_bytes_from_pcm(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;

    let data_len = samples.len() as u32 * 2;
    let byte_rate = sample_rate * CHANNELS as u32 * (BITS as u32 / 8);
    let block_align = CHANNELS * (BITS / 8);

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());

    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    /// A stereo device's frames fold to single samples, averaging the channels
    /// so a microphone that lands on either side is still recorded.
    #[test]
    fn stereo_frames_average_into_mono() {
        let interleaved = [0.5, -0.5, 1.0, 0.0, 0.25, 0.25];
        let mono = to_mono(&interleaved, 2);
        assert_eq!(mono, vec![0.0, 0.5, 0.25]);
    }

    /// A partial trailing frame is not a frame: it is dropped rather than
    /// averaged into a shorter sample and shifting every later one.
    #[test]
    fn a_short_tail_is_dropped_not_averaged() {
        let mono = to_mono(&[0.1, 0.3, 0.9], 2);
        assert_eq!(mono, vec![0.2]);
    }

    /// Mono input passes through untouched, and a bogus channel count of zero
    /// is read as mono instead of dividing by zero.
    #[test]
    fn mono_passes_through_and_zero_channels_is_mono() {
        let samples = [0.1, -0.2, 0.3];
        assert_eq!(to_mono(&samples, 1), samples.to_vec());
        assert_eq!(to_mono(&samples, 0), samples.to_vec());
    }

    /// 48 kHz to 16 kHz divides by three and keeps the signal's shape: same
    /// endpoints, three times fewer samples.
    #[test]
    fn resample_48k_to_16k_thirds_the_length() {
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 / 48_000.0).sin()).collect();
        let out = resample(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!((out[0] - input[0]).abs() < 1e-6);
        assert!((out[out.len() - 1] - input[input.len() - 1]).abs() < 1e-3);
    }

    /// 44.1 kHz is the awkward one: the step is fractional, so the output
    /// length must still follow the ratio and never run past the input.
    #[test]
    fn resample_44k1_to_16k_follows_the_ratio() {
        let input: Vec<f32> = (0..44_100).map(|i| i as f32).collect();
        let out = resample(&input, 44_100, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|s| *s <= 44_099.0));
    }

    /// Equal rates are a copy, and an empty recording or a zero rate is empty
    /// output instead of a panic.
    #[test]
    fn resample_edge_cases() {
        let input = [0.1, 0.2, 0.3];
        assert_eq!(resample(&input, 16_000, 16_000), input.to_vec());
        assert!(resample(&[], 48_000, 16_000).is_empty());
        assert!(resample(&input, 0, 16_000).is_empty());
        assert!(resample(&input, 48_000, 0).is_empty());
    }

    /// The WAV is a canonical mono PCM16 file: every header field the decoders
    /// read has to agree with the sample count, or the endpoint answers with a
    /// 400 instead of a transcript.
    #[test]
    fn wav_header_describes_the_samples() {
        let samples = [0.0, 0.5, -0.5];
        let wav = wav_bytes(&samples, TARGET_RATE);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32_at(&wav, 16), 16, "PCM fmt chunk length");
        assert_eq!(u16_at(&wav, 20), 1, "format 1 is PCM");
        assert_eq!(u16_at(&wav, 22), 1, "mono");
        assert_eq!(u32_at(&wav, 24), TARGET_RATE);
        assert_eq!(u32_at(&wav, 28), TARGET_RATE * 2, "byte rate");
        assert_eq!(u16_at(&wav, 32), 2, "block align");
        assert_eq!(u16_at(&wav, 34), 16, "bits per sample");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(&wav, 40), samples.len() as u32 * 2, "data length");
        assert_eq!(u32_at(&wav, 4), wav.len() as u32 - 8, "RIFF chunk size");
        assert_eq!(wav.len(), 44 + samples.len() * 2);
    }

    /// Silence, full scale and the two poles map to the PCM16 values a decoder
    /// expects, and anything past full scale is clipped rather than wrapped
    /// into the opposite polarity.
    #[test]
    fn wav_samples_are_pcm16_and_clipped() {
        let wav = wav_bytes(&[0.0, -1.0, 1.0, 1.5, -1.5], 16_000);
        let sample_at = |i: usize| i16::from_le_bytes([wav[44 + i * 2], wav[44 + i * 2 + 1]]);
        assert_eq!(sample_at(0), 0);
        assert_eq!(sample_at(1), i16::MIN + 1);
        assert_eq!(sample_at(2), i16::MAX);
        assert_eq!(sample_at(3), i16::MAX, "above full scale clips");
        assert_eq!(sample_at(4), i16::MIN + 1, "below full scale clips");
    }

    /// An empty recording still produces a header a decoder can parse, so the
    /// failure path is a clear "no audio" rather than a malformed request.
    #[test]
    fn an_empty_recording_is_a_valid_header_and_nothing_else() {
        let wav = wav_bytes(&[], TARGET_RATE);
        assert_eq!(wav.len(), 44);
        assert_eq!(u32_at(&wav, 40), 0);
    }

    /// The meter is a fixed window that scrolls: the oldest reading goes when
    /// the strip is full, and a reading past full scale or below silence is
    /// clamped so the waveform can never paint outside its box.
    #[test]
    fn the_meter_keeps_a_fixed_window_and_clamps() {
        let meter: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        for i in 0..(METER_BARS + 10) {
            push_level(&meter, i as f32 / 100.0);
        }
        let levels = meter.lock().map(|g| g.clone()).unwrap_or_default();
        assert_eq!(levels.len(), METER_BARS, "the window never grows");
        assert_eq!(levels[0], 0.10, "the oldest ten readings were dropped");
        assert_eq!(levels[METER_BARS - 1], (METER_BARS + 9) as f32 / 100.0);

        push_level(&meter, 4.0);
        push_level(&meter, -2.0);
        let levels = meter.lock().map(|g| g.clone()).unwrap_or_default();
        assert_eq!(levels[METER_BARS - 2], 1.0, "above full scale clamps");
        assert_eq!(levels[METER_BARS - 1], 0.0, "below zero clamps to silence");
    }
}
