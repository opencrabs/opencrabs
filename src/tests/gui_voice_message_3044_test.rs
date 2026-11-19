//! A recording is a message, start to finish (#3044).
//!
//! The window records through the microphone, saves the take as an OGG/Opus
//! file beside the profile, attaches the clip and its words to the composer,
//! and only sends when the person presses Enter. The turn then carries a
//! `<<AUD:path>>` marker and the answer is spoken back when it settles. That is
//! the loop Telegram has always run, and it only holds together while five
//! rules do:
//!
//! * the stop control ends the recording and previews the take, because a stop
//!   is not a decision to send;
//!
//! * the take is previewed above the row with its own wave and transcript, and
//!   it is spent by the send that carries it, so a clip never rides the next
//!   message;
//! * the marker carries the words and the clip in one string, and the send path
//!   splits them so the agent reads prose and never a path;
//! * a recording is never mistaken for a reply, or a spoken answer would be
//!   offered back to the microphone;
//! * the marker is a display cue, so it is stripped before a stored row becomes
//!   context, or the model learns to emit markers of its own;
//! * the control that started the recording becomes the thing that stops it.
//!
//! The window has no headless harness, so the drawing rules are read from
//! source the way the other gui tests read them. The marker strip itself is
//! library code and is exercised directly.

const EDITOR: &str = include_str!("../gui/editor.rs");
const VOICE_INPUT: &str = include_str!("../gui/voice_input.rs");
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const FINDER: &str = include_str!("../gui/dashboard_finder.rs");
const IMAGE_UTILS: &str = include_str!("../utils/image.rs");

/// The source between two of its neighbours, so an assertion cannot be
/// satisfied by an unrelated function elsewhere in the file.
fn region_in(source: &'static str, start: &str, end: &str) -> &'static str {
    source
        .split(start)
        .nth(1)
        .unwrap_or_else(|| panic!("{start} is defined"))
        .split(end)
        .next()
        .unwrap_or_else(|| panic!("{end} follows {start}"))
}

fn editor(start: &str, end: &str) -> &'static str {
    region_in(EDITOR, start, end)
}

/// One string carries both halves of a voice message, and the send path takes
/// it apart: the words go to the agent, the marker stays on the row that draws
/// the player. A recording nobody spoke into has no question to answer, so it
/// travels whole instead of arriving as an empty turn.
#[test]
fn a_voice_message_travels_as_words_plus_one_marker() {
    let split = editor("pub fn split_audio_marker(", "pub fn strip_markers(");
    assert!(
        split.contains("kind == \"AUD\""),
        "only an audio marker splits, so an image still rides with its text"
    );
    assert!(
        split.contains("if words.is_empty()"),
        "a silent take has nothing to answer and keeps its marker"
    );
    assert!(
        split.contains("Some((words, format!(\"<<{marker}>>\")))"),
        "the two halves come back separately: words for the agent, marker for the row"
    );
}

/// The stop control ends the take, it does not send it. The clip and the words
/// wait in the composer's own preview, where they can be heard and read back
/// before Enter spends them (#3044).
#[test]
fn the_stop_attaches_the_take_and_leaves_the_send_to_the_user() {
    let land = editor("fn land_voice(", "/// Throw away the take");
    assert!(
        land.contains("self.voice_draft = Some(VoiceDraft {"),
        "the take waits in the composer's preview instead of going out"
    );
    assert!(
        !land.contains("EditorEvent::Sent"),
        "a stop is not a send: Enter is the one thing that emits the message"
    );
    // A transcription whose clip could not be saved still has words worth
    // sending, and the field is the only place left for them.
    assert!(
        land.contains("self.insert_dictation(&spoken.text, cx)"),
        "a take with no clip keeps its words in the field rather than losing them"
    );
}

/// A stopped take is a message the user still holds, so it is drawn where it
/// can be read back before it is spent: under any attachment chip, above the
/// input row, with the wave and the length of the take it came from (#3044).
#[test]
fn the_take_previews_above_the_row_with_its_own_shape() {
    let render = EDITOR
        .split("fn render(")
        .nth(1)
        .and_then(|rest| rest.split("fn attachment_strip(").next())
        .expect("editor render");
    let clip_at = render
        .find("body.child(attachment_strip(&self.attachments, cx))")
        .expect("the chips are drawn before the row");
    let voice_at = render
        .find("body.child(voice_strip(draft, cx))")
        .expect("the take is drawn before the row");
    let row_at = render
        .find(r#".id("composer-input")"#)
        .expect("the row builds the input");
    assert!(
        clip_at < voice_at && voice_at < row_at,
        "a take with a chip attached reads chip, then take, then the words"
    );

    let strip = editor("fn voice_strip(", "/// The keybindings the editor needs");
    assert!(
        strip.contains("crate::voice_preview::player(")
            && strip.contains("&draft.clip")
            && strip.contains("Some(draft.seconds)")
            && strip.contains("&draft.levels"),
        "the take uses the shared player and keeps its capture as the decode fallback"
    );
    assert!(
        strip.contains("\"voice-preview-discard\""),
        "the ✕ is its own element, so throwing a take away cannot also move the caret"
    );
    assert!(
        !strip.contains("EditorEvent::Sent"),
        "the preview holds the message; Enter is still the only thing that sends it"
    );
}

/// Enter spends the take exactly once. A second Enter on the empty composer
/// must not send the same words and the same clip again, which is what the
/// transfer out of the field is for.
#[test]
fn one_send_spends_the_take() {
    let send = editor("fn send(", "fn payload_with_reply(");
    assert!(
        send.contains("let voice = self.voice_draft.take();"),
        "the take leaves the field with the message it belongs to"
    );
    assert!(
        send.contains("voice.is_none()"),
        "a take alone is something to send, even with no character typed"
    );
    assert!(
        send.contains("<<AUD:{}>>"),
        "the clip goes out as the marker the row draws as a player"
    );
}

/// Throwing a take away is one action: the draft goes, the microphone state
/// goes, and the player stops singing a clip the composer no longer shows.
#[test]
fn the_take_can_be_thrown_away() {
    let discard = editor("fn discard_voice(", "fn insert_dictation(");
    assert!(
        discard.contains("self.voice_draft.take()"),
        "the draft is consumed rather than left behind"
    );
    assert!(
        discard.contains("crate::voice_play::stop(&draft.clip)"),
        "a discarded take stops outright, including a paused one"
    );
    assert!(
        discard.contains("self.voice_levels.clear()"),
        "the meter readings belong to the discarded take"
    );
    // A take is only replaced by sending or discarding it: a second recording
    // would land on top of words the person can still read (#3044).
    let toggle = editor("fn toggle_dictation(", "fn pump_voice(");
    assert!(
        toggle.contains("self.voice_draft.is_some()"),
        "the microphone refuses to start over a take still in the preview"
    );
}

/// A take belongs to the message and the conversation it was recorded for. It
/// is dropped wherever the field is emptied, or a clip recorded in one
/// conversation would arrive in a different one (#3044).
#[test]
fn the_take_never_rides_the_next_message() {
    let clear = editor("fn clear_after_send(", "fn ");
    assert!(
        clear.contains("self.voice_draft = None"),
        "an emptied field cannot leave a take waiting for the next Enter"
    );
    let switch = editor("pub fn begin_conversation(", "pub fn ");
    assert!(
        switch.contains("self.voice_draft = None"),
        "the take follows the attachments out: a new conversation starts clean"
    );
    let empty = editor("pub fn is_empty(", "pub fn discard_draft(");
    assert!(
        empty.contains("self.voice_draft.is_none()"),
        "a take is a draft, so Ctrl+C has something to throw away"
    );
}

/// The words reach the agent and the marker reaches the row. Both send paths,
/// the composer and a split pane, have to agree, or a recording sent from a
/// pane loses its clip.
#[test]
fn the_agent_reads_the_words_and_the_row_keeps_the_clip() {
    let send = region_in(DASHBOARD_TURNS, "fn send_from_pane(", "fn ");
    assert!(
        send.contains("editor::split_audio_marker(&original)"),
        "the send path asks the editor to split the marker off the draft"
    );
    assert!(
        send.contains("self.voice_turns.insert(session)"),
        "the turn is remembered as spoken, so the settle can answer out loud"
    );
    // The agent cannot play the clip, so the words have to arrive labelled as
    // speech: a transcript handed over as ordinary prose gets an ordinary
    // answer, and the whole point of a voice note is the spoken reply.
    assert!(
        send.contains("editor::VOICE_NOTE_PREFIX"),
        "the prompt says the words were spoken"
    );
    assert!(
        send.contains("format!(\"{spoken} {marker}\")"),
        "the note goes to the agent only: the row shows the words and the clip"
    );
    assert!(
        region_in(DASHBOARD_TURNS, "fn send_message_showing(", "fn ")
            .contains("self.voice_turns.insert(sid)"),
        "a message shown in a pane takes the same path"
    );
}

/// A take and a reply live in the same folder, so the name is the only thing
/// that tells them apart. Without the prefix the player would offer the agent's
/// own voice back to the microphone.
#[test]
fn a_recording_and_a_spoken_reply_keep_different_names() {
    let input = VOICE_INPUT;
    assert!(
        input.contains("const RECORDING_PREFIX: &str = \"voice-\";"),
        "a take is named for what it is"
    );
    assert!(
        input.contains("const REPLY_PREFIX: &str = \"reply-\";"),
        "an answer is named for what it is"
    );
    let recording = region_in(input, "pub(crate) fn is_recording(", "/// Write one take");
    assert!(
        recording.contains(".starts_with(RECORDING_PREFIX)"),
        "only a take counts as a recording, and the folder alone cannot say so"
    );
    let reply = region_in(input, "pub(crate) fn save_reply_audio(", "fn write_clip(");
    assert!(
        reply.contains("write_clip(ogg, REPLY_PREFIX)"),
        "the synthesized answer is written under the reply prefix"
    );
}

/// The card is chosen from the extension, and a video is never one: an `.ogg`
/// plays where it sits, and a `.mp4` still opens.
#[test]
fn only_an_audio_extension_draws_a_player() {
    let listing = editor(
        "const AUDIO_EXTENSIONS: &[&str]",
        "/// One row of the composer",
    );
    for extension in [".ogg", ".opus", ".mp3", ".wav", ".m4a"] {
        assert!(
            listing.contains(&format!("\"{extension}\"")),
            "{extension} has to classify as audio, or its clip draws as a file"
        );
    }

    let is_audio = editor("pub fn is_audio(", "/// The marker the agent reads");
    assert!(
        is_audio.contains("if self.is_video {"),
        "a video is not audio, even when its container could carry sound"
    );

    let marker = editor(
        "pub fn marker(&self)",
        "/// The text that actually goes to the agent",
    );
    assert!(
        marker.contains("} else if self.is_audio() {"),
        "audio emits AUD rather than borrowing IMG, which draws a picture"
    );

    let cards = region_in(TRANSCRIPT, "fn history_attachments(", "fn attachment_cell(");
    assert!(
        cards.contains(".partition(") && cards.contains(".is_audio()"),
        "audio is separated from the square attachment plan"
    );
    assert!(
        cards.contains("crate::voice_preview::player("),
        "the stored clip uses the same seekable player as the composer"
    );
}

/// The marker is a cue for the transcript, not something either side said. A
/// stored row is cleaned before it becomes context, or the model reads its own
/// marker back and learns to write one.
#[test]
fn the_marker_never_comes_back_as_context() {
    let extractor = region_in(
        IMAGE_UTILS,
        "pub fn extract_aud_markers(",
        "/// Extract `<<react:emoji>>",
    );
    assert!(
        extractor.contains("extract_markers_with_prefix(text, \"<<AUD:\")"),
        "the strip reuses the marker walk the image and video markers already use"
    );

    let (words, clips) =
        crate::utils::extract_aud_markers("Here is what I said.\n\n<<AUD:/tmp/voice/voice-1.ogg>>");
    assert_eq!(
        words, "Here is what I said.",
        "the words survive the strip, because they are the message"
    );
    assert_eq!(
        clips,
        vec!["/tmp/voice/voice-1.ogg".to_string()],
        "the path is recovered for whatever needs to play it"
    );
}

/// The meter shows the recent past, not the whole take: a fixed window of the
/// newest readings, folded to the range a bar can draw, with a floor so silence
/// still looks like a live strip.
#[test]
fn the_wave_is_a_fixed_window_of_the_recent_past() {
    let wave = editor("fn waveform(", "/// The keybindings the editor needs");
    assert!(
        wave.contains("levels.len().saturating_sub(WAVE_BARS)"),
        "the window ends at the newest reading, so the wave moves under the voice"
    );
    assert!(
        wave.contains("level.clamp(0.0, 1.0) * 12.0"),
        "a sample louder than full scale must not draw past the strip"
    );
    assert!(
        wave.contains("px(2.0 +"),
        "every bar keeps a floor, or a quiet take reads as a dead microphone"
    );
    assert!(
        EDITOR.contains("const WAVE_BARS: usize = 34;"),
        "the window has one size, shared by the drawing and the buffer"
    );
}

/// One control whose meaning follows the state, which is how every recorder
/// works: while the microphone is open it counts, draws the wave, and offers
/// the stop square in place of the microphone.
#[test]
fn the_microphone_becomes_the_thing_that_stops_it() {
    let mic = editor("fn mic(", "/// How many meter readings");
    assert!(
        mic.contains("\"editor-mic\""),
        "the control keeps the id the composer row and the tests look for"
    );
    assert!(
        mic.contains("if phase == VoicePhase::Recording"),
        "the recording strip only exists while the microphone is open"
    );
    assert!(
        mic.contains("waveform(levels, palette.red)"),
        "the strip shows what the device is hearing"
    );
    assert!(
        mic.contains("crate::icon::stop_icon(colour, 1.05)"),
        "the mark is the stop square, not a second button to find"
    );
    assert!(
        mic.contains("secs / 60") && mic.contains("secs % 60"),
        "the counter says how long, which a red mark alone never does"
    );
}

/// Escape stops the recording before anything else reads the key, and a click
/// on the control takes the same path: every way out of a take goes through the
/// one stop.
#[test]
fn escape_ends_a_recording_before_anything_else() {
    let keys = editor("fn intercept_raw_key(", "fn line_count(");
    let recording = keys
        .find("self.voice_phase() == VoicePhase::Recording")
        .expect("escape knows the microphone is open");
    let selection = keys
        .find("self.marked_range.is_some()")
        .expect("escape still clears a selection");
    let menu = keys
        .find("self.ctx_menu_at.take()")
        .expect("escape still closes a card");
    assert!(
        recording < selection && recording < menu,
        "the microphone is checked ahead of the selection and the card, because a key mid-take means stop"
    );
    assert!(
        keys[recording..]
            .split("return;")
            .next()
            .is_some_and(|arm| arm.contains("self.toggle_dictation(cx)")),
        "the same toggle that started the recording ends it, click or key"
    );
}

/// A turn that began as a recording is answered out loud. The settle asks for
/// synthesis only for those turns, so a typed question never gets a voice
/// answer nobody asked for.
#[test]
fn a_spoken_turn_is_answered_aloud() {
    let settle = DASHBOARD_TURNS;
    assert!(
        settle.contains("self.voice_turns.remove(&session)"),
        "the settle consumes the flag, so the next typed turn stays quiet"
    );
    let speak = region_in(AGENT_BRIDGE, "pub fn spawn_speak_reply(", "\n}");
    assert!(
        speak.contains("channels::voice::synthesize"),
        "the window speaks through the same synthesis the channels use"
    );
    assert!(
        speak.contains("save_reply_audio"),
        "the answer is saved beside the take so the row can play it"
    );
    assert!(
        speak.contains("<<AUD:{}>>"),
        "the answer row carries its clip the way a recording does"
    );
    assert!(
        speak.contains("tts_enabled"),
        "a profile with no synthesis engine answers in text, as it does on Telegram"
    );
    assert!(
        region_in(AGENT_BRIDGE, "pub fn spawn_speak_reply(", "\n}").contains("Builder::new()"),
        "the synthesis runs on its own stack: gpui workers are far too small for a provider call"
    );
}

/// The take's colour is the glass itself, not a caption over it: the orb
/// worker is handed amber while the device is open and green from the hand-off
/// to the transcriber, which is what lets the bubble stay bare (#3044).
#[test]
fn the_glass_carries_the_phase_colour() {
    let view = region_in(
        FINDER,
        "pub(crate) fn seeker_voice_view(",
        "/// Retire the currently",
    );
    assert!(
        view.contains("let colour = if recording { theme.amber } else { theme.green };"),
        "yellow while the device is open, green from the hand-off to the transcriber"
    );
    assert!(
        view.contains("orb.update(recording, &self.seeker_voice_levels, colour)"),
        "the colour reaches the material, so nothing has to be drawn over the glass"
    );
}
