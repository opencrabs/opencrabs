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
//! * the take is previewed above the row as glass, with its own shape and
//!   transcript, and it is spent by the send that carries it, so a clip never
//!   rides the next message;
//! * the marker carries the words and the clip in one string, and the send path
//!   splits them so the agent reads prose and never a path;
//! * a recording is never mistaken for a reply, or a spoken answer would be
//!   offered back to the microphone;
//! * the marker is a display cue, so it is stripped before a stored row becomes
//!   context, or the model learns to emit markers of its own;
//! * the control that started the recording becomes the thing that stops it;
//! * and the four ways out of a take stay distinct (#3092): Enter stops a take
//!   and keeps it, the chord stops it and sends, a click parks the words for
//!   reading, and escape throws the take away.
//!
//! The window has no headless harness, so the drawing rules are read from
//! source the way the other gui tests read them. The marker strip itself is
//! library code and is exercised directly.

const EDITOR: &str = include_str!("../gui/editor.rs");
const VOICE_INPUT: &str = include_str!("../gui/voice_input.rs");
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
const TURN: &str = include_str!("../backend/turn.rs");
const SPEAK: &str = include_str!("../backend/speak.rs");
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
        strip.contains("crate::composer_orb::player(")
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

/// Enter stops a take and keeps it (#3092).
///
/// The key used to reach the send path with the microphone still open, where an
/// empty field had nothing to send and the press did nothing at all. That left
/// the mouse as the only way to keep a take without sending it. Enter parks the
/// take instead, so the words land in the preview beside anything typed there
/// and the next Enter is the send.
#[test]
fn enter_stops_a_take_and_keeps_it() {
    let send = editor("fn send(", "fn submit(");
    let phase = send
        .find("if self.voice_phase() != VoicePhase::Idle")
        .expect("enter knows the microphone is open");
    let park = send
        .find("self.toggle_dictation(TakeEnd::Park, cx)")
        .expect("the press parks the take instead of dropping it");
    assert!(
        phase < park,
        "the phase is read first, so a take in flight never reaches the emoji list or the send"
    );
    assert!(
        // `find` lands on a char boundary, so this cannot fail, but `get` keeps
        // the slice off the panic path and the ratchet honest (#3096).
        send.get(phase..)
            .and_then(|rest| rest.split("return;").next())
            .is_some_and(|arm| arm.contains("self.toggle_dictation(TakeEnd::Park, cx)")),
        "the arm hands the key over and stops: a park that fell through would send an empty field"
    );
    assert!(
        !send.contains("cx.emit(EditorEvent::Sent"),
        "Enter on a recording sends nothing, because the words are still being made"
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
    // The decision travels with the turn itself (#3096). A flag keyed by
    // session outlived the turn that set it, and the next typed question in
    // that conversation was answered in audio nobody asked for.
    assert!(
        send.contains("voice.is_some(),"),
        "the send hands the turn its own spoken flag"
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
        region_in(DASHBOARD_TURNS, "fn send_message_showing(", "fn ").contains("voice.is_some(),"),
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
        reply.contains("write_clip(ogg, REPLY_PREFIX, session)"),
        "the synthesized answer is written under the reply prefix"
    );
}

/// A clip follows its conversation (#3114).
///
/// The take used to land in the profile's `voice/` drawer whatever the session
/// was, so a project's recordings piled up in one folder the project never
/// showed. The folder is now the session's project attachments dir, through the
/// same resolver every other attachment uses, and only a session with no
/// project falls back to `voice/`.
#[test]
fn a_take_lands_in_its_project_and_falls_back_to_the_voice_folder() {
    let input = VOICE_INPUT;
    let clip_dir = region_in(input, "fn clip_dir(", "/// Write one take");
    assert!(
        clip_dir.contains("project_clip_dir(session)"),
        "the project folder is asked for first"
    );
    assert!(
        clip_dir
            .contains(".unwrap_or_else(|| opencrabs::config::opencrabs_home().join(VOICE_DIR))"),
        "a session with no project still has somewhere to put its take"
    );
    let resolve = region_in(input, "fn project_clip_dir(", "/// Write one take");
    assert!(
        resolve.contains("FileService::new"),
        "the clip goes where every other attachment goes, through the file service"
    );
    assert!(
        resolve.contains(".project_files_dir(session)"),
        "the attachments dir is the project's own resolver, not a second guess at the slug"
    );
    assert!(
        resolve.contains("global_pool()"),
        "the capture thread reuses the process's open pool instead of connecting again"
    );
    // The capture path has to hand the session down, or the resolver above has
    // nothing to resolve: the take is transcribed on its own thread.
    assert!(
        input.contains("pub(crate) fn start(recording: Recording, session: Option<Uuid>) -> Self"),
        "the transcription job carries the conversation it belongs to"
    );
    assert!(
        input.contains("save_voice_note(&recording.pcm, session)"),
        "the save is told which conversation the take came from"
    );
    assert!(
        editor("voice_input::Transcription::start(", "));").contains("self.session"),
        "the composer hands its own session to the take it starts"
    );
    let speak = region_in(AGENT_BRIDGE, "pub fn spawn_speak_reply(", "\n}");
    assert!(
        speak.contains("save_reply_audio(&audio, Some(session_id))"),
        "a spoken answer lands beside the take that asked for it"
    );
    // A take is now readable from two homes, so the folder test admits both.
    let recording = region_in(
        input,
        "pub(crate) fn is_recording(",
        "/// The folder a clip",
    );
    assert!(
        recording.contains("ProjectService::projects_dir()"),
        "a clip in a project's attachments folder is still a take"
    );
    assert!(
        recording.contains("profile.join(VOICE_DIR)"),
        "and so is one in the profile's own voice folder"
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
        cards.contains("crate::composer_orb::player_plain("),
        "the stored clip uses the same glass block as the composer"
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

/// The glass is the composer now (#3092): the take's own peaks reach the block
/// on every tick of the dictation loop, and the block lives exactly as long as
/// the microphone does. A worker that outlived its take would keep painting a
/// recording that already closed.
#[test]
fn the_glass_is_fed_the_take_and_closes_with_it() {
    let poll = editor("fn poll_voice(", "fn land_voice(");
    assert!(
        poll.contains("block.update(") && poll.contains("&self.voice_levels"),
        "the block stopped being handed the take"
    );
    assert!(
        poll.contains("self.voice_block.take()") && poll.contains("block.dispose(cx)"),
        "the block has to be retired when the capture ends, not on the next take"
    );
    let start = editor("pub(crate) fn toggle_dictation(", "fn pump_voice(");
    assert!(
        start.contains("LiveSlab::start("),
        "the block opens with the microphone"
    );
    // Assembled rather than written out, so the assertion cannot match its own
    // text: a source scan that finds itself proves nothing.
    let bar_strip = ["fn ", "wave", "form("].concat();
    assert!(
        !EDITOR.contains(&bar_strip),
        "the bar strip was replaced by the glass, not stacked beside it"
    );
}

/// The block is what stops the take, and it is the whole composer while the
/// microphone is open: the strip in the row was the version the owner could not
/// see, and the takeover is the point of it.
#[test]
fn the_block_is_the_thing_that_stops_it() {
    let block = editor("fn voice_block(", "fn voice_strip(");
    assert!(
        block.contains("crate::composer_orb::BLOCK_ID"),
        "the block carries the id the composer row and the tests look for"
    );
    assert!(
        block.contains("block.frame(cx)"),
        "the composer draws the block, not a bar strip"
    );
    assert!(
        block.contains("secs / 60") && block.contains("secs % 60"),
        "the counter says how long, which a red mark alone never does"
    );
    assert!(
        block.contains("this.toggle_dictation(TakeEnd::Park, cx)"),
        "a surface that draws a stop and answers nothing is worse than none"
    );
    assert!(
        block.contains("measure_width"),
        "a block that never learns its width is drawn at the wrong aspect"
    );
    // The row's microphone is only the idle mark now: a take is not stopped
    // from a glyph the size of a full stop.
    let mic = editor("fn mic(", "fn voice_block(");
    assert!(
        mic.contains("\"editor-mic\""),
        "the idle control keeps the id the composer row and the tests look for"
    );
    assert!(
        mic.contains("crate::icon::mic_icon("),
        "the idle control still shows the microphone"
    );
}

/// Escape throws the take away before anything else reads the key, and every
/// other way out of a recording parks it: stop and abandon are two different
/// decisions, and the key that ends a take mid-sentence is the one that means
/// abandon.
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
            .is_some_and(|arm| arm.contains("self.toggle_dictation(TakeEnd::Cancel, cx)")),
        "the same toggle that started the recording ends it, and escape cancels rather than attaching"
    );
}

/// A take thrown away leaves nothing behind, and that has to survive the two
/// threads between the key and the words: the capture still has a WAV on the
/// way, and a cancellation that only stopped the device would come back as the
/// preview escape was pressed to avoid.
#[test]
fn a_cancelled_take_is_never_transcribed_or_previewed() {
    let poll = editor("fn poll_voice(", "fn land_voice(");
    assert!(
        poll.contains("let cancelled = std::mem::take(&mut self.voice_cancel_on_land)"),
        "the abandonment is spent when the recording comes back"
    );
    assert!(
        poll.contains("Ok(_) if cancelled => {}"),
        "a cancelled recording is dropped instead of being transcribed"
    );
    assert!(
        poll.contains("if std::mem::take(&mut self.voice_cancel_on_land) {"),
        "an escape that arrives while the words are being made still wins"
    );
    // The three gestures that keep the take all park, so escape stays the only
    // way out of a recording that throws it away.
    let block = editor("fn voice_block(", "fn voice_strip(");
    assert!(
        block.contains("this.toggle_dictation(TakeEnd::Park, cx)"),
        "the block's click still parks: stopping is not abandoning"
    );
    let mic = editor("fn mic(", "fn voice_block(");
    assert!(
        mic.contains("this.toggle_dictation(TakeEnd::Park, cx)"),
        "the microphone's click still parks, so only escape discards a take"
    );
}

/// A turn that began as a recording is answered out loud. The settle asks for
/// synthesis only for those turns, so a typed question never gets a voice
/// answer nobody asked for.
#[test]
fn a_spoken_turn_is_answered_aloud() {
    let settle = DASHBOARD_TURNS;
    assert_eq!(
        settle.matches("if voice {").count(),
        2,
        "both settles speak for the turn's own flag, the leaf's and the window's"
    );
    assert!(
        !settle.contains("voice_turns"),
        "no session keyed flag is left for a later turn to inherit"
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
    // The row is the crab's since L10: the window hands the clip to the
    // backend's op, which writes the marker.
    assert!(
        speak.contains("speak::AttachVoiceReply"),
        "the window puts the clip on the row through the backend"
    );
    assert!(
        SPEAK.contains("<<AUD:{}>>"),
        "the answer row carries its clip the way a recording does"
    );
    assert!(
        speak.contains("tts_enabled"),
        "a profile with no synthesis engine answers in text, as it does on Telegram"
    );
    // The synthesis needs a stack of its own: gpui workers are far too small
    // for a provider call. Since #3033 the thread is built by the lane helper,
    // so the speak lane asks for one instead of spelling out the builder.
    assert!(
        speak.contains("spawn_worker(\"gui-voice-reply\""),
        "the synthesis asks the lane helper for a worker thread"
    );
    assert!(
        region_in(AGENT_BRIDGE, "pub(crate) fn spawn_worker<F>(", "\n}").contains("Builder::new()"),
        "the lane helper is where the thread and its stack come from"
    );
}

/// A turn speaks only for the input that started it (#3096).
///
/// The speech decision used to live in a set keyed by session. A voice turn
/// whose settle never arrived left that conversation flagged, so the next
/// TYPED question in it came back as audio nobody asked for. The flag now
/// rides the turn and returns on the event that settles it, and a text turn
/// has nothing to inherit.
#[test]
fn a_text_turn_never_inherits_a_voice_answer() {
    assert!(
        !DASHBOARD_TURNS.contains("voice_turns"),
        "no session keyed set survives for a later turn to inherit"
    );
    let done = region_in(AGENT_BRIDGE, "    Done {", "    /// The turn failed");
    assert!(
        done.contains("voice: bool"),
        "the settle is told whether this turn's own input was a recording"
    );
    let spawn = region_in(AGENT_BRIDGE, "pub fn spawn_turn(", "let thread_sink");
    assert!(
        spawn.contains("voice: bool"),
        "the turn takes the flag from the send that started it"
    );
    // A drifting model that writes a spoken block on a text turn is stripped
    // here, before the settle draws the answer.
    let settle = region_in(TURN, "match outcome {", "Err(e) =>");
    assert!(
        settle.contains("spoken::visible(&response.content)"),
        "a reply on a text turn is drawn without its spoken block"
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

/// The chord sends, and the microphone opening silences the speaker (#3092).
///
/// Two promises ride on the same gesture, and both are read from source
/// because the window has no headless harness. The first is speed: pressing the
/// chord a second time puts the take on the wire instead of parking it in the
/// preview, which is what makes dictation usable without reaching for Enter.
/// The second is that the take is clean: a reply still sounding when the
/// device opens would be recorded under the person's voice, so playback ends
/// before the capture starts, on the composer and on the Seeker's own row.
#[test]
fn the_chord_sends_and_the_microphone_silences_the_speaker() {
    let chord = editor("fn toggle_voice(", "pub(crate) fn toggle_dictation(");
    assert!(
        chord.contains("self.toggle_dictation(TakeEnd::Send, cx)"),
        "the chord is the fast path: it asks for a send, not a park"
    );
    // Order matters more than presence here: a silence after the device is
    // open would cut a reply the take has already recorded.
    let open = editor("pub(crate) fn toggle_dictation(", "fn pump_voice(");
    let silenced = open
        .find("crate::voice_play::silence()")
        .expect("the composer silences playback when the take opens");
    let started = open
        .find("voice_input::Capture::start()")
        .expect("the composer opens the device");
    assert!(
        silenced < started,
        "the speaker steps aside before the microphone is open, never after"
    );
    let seeker = region_in(FINDER, "pub(crate) fn seeker_dictate(", "\n    }\n");
    let quiet = seeker
        .find("crate::voice_play::silence()")
        .expect("the Seeker's row silences playback too");
    let opens = seeker
        .find("voice_input::Capture::start()")
        .expect("the Seeker's row opens the device");
    assert!(
        quiet < opens,
        "the same order on the card: quiet first, then listen"
    );
}

/// The intent outlives the press, and dies everywhere a take can end (#3092).
///
/// The stop and the words are two worker threads apart, so the decision is
/// written down and spent later. Spending it exactly once is what keeps a
/// single press from sending twice, and clearing it on every other way out is
/// what keeps it from arming the next take: a device that failed, a field that
/// was emptied, and a conversation that was left are all ends without a send.
#[test]
fn the_send_intent_is_spent_once_and_cleared_everywhere_else() {
    let landing = editor("fn land_voice(", "fn discard_voice(");
    assert!(
        landing.contains("if send {") && landing.contains("self.submit(cx)"),
        "the take is parked and then sent through the one send path"
    );
    let poll = editor("fn poll_voice(", "fn land_voice(");
    assert!(
        poll.contains("std::mem::take(&mut self.voice_send_on_land)"),
        "the intent is taken, so a second tick cannot send the same words again"
    );
    let clear = editor("fn clear_after_send(", "pub fn begin_conversation(");
    assert!(
        clear.contains("self.voice_send_on_land = false"),
        "an emptied field carries no intent into the next message"
    );
    let switch = editor("pub fn begin_conversation(", "pub fn ");
    assert!(
        switch.contains("self.voice_send_on_land = false"),
        "leaving the conversation drops the press with the draft"
    );
}

/// The note the model reads never reaches a screen (#3092).
///
/// A voice turn's prompt is the transcript with `[voice note]` in front of it,
/// because a model handed plain prose answers plain prose. The reader who spoke
/// the words is not the model, so both ways a card starts strip it: a send, and
/// a turn a restart resumed from the row the previous process wrote.
#[test]
fn the_note_that_tells_the_model_reaches_no_card() {
    let begin = region_in(
        TRANSCRIPT,
        "pub fn begin(&mut self, user: String)",
        "pub fn resume(&mut self, user: String)",
    );
    assert!(
        begin.contains("spoken::without_prefix(&user)"),
        "the card strips the note where the turn starts, so a resumed turn does too"
    );
    // A resumed turn draws the question the ledger holds. The parked prompt is
    // words only, which is what put the note on the card and left the clip with
    // no player; the row carries both halves.
    let resume = region_in(
        AGENT_BRIDGE,
        "let user_message = match ledger_question(",
        "let cancel = tokio_util::sync::CancellationToken::new();",
    );
    assert!(
        resume.contains("&messages, session_id).await"),
        "boot resume asks the ledger for the question instead of drawing the prompt"
    );
    assert!(
        AGENT_BRIDGE.contains("async fn ledger_question("),
        "the lookup is one function, so both halves of the question travel together"
    );
}

/// A marker is the message's own attachment only at the tail of its line (#3092).
///
/// The send path appends markers after the words. Reading every marker anywhere
/// in a body made a reply that quoted one draw a player for a clip it never sent,
/// and a quote shortened with an ellipsis opened nothing when pressed.
///
/// The tail is read per line rather than per body, because a queued message is
/// spliced onto the one before it with a newline and its markers end its own line
/// (#3099). Both readers below take their spans from that one place, so the text
/// and the tiles cannot disagree about which markers are attachments.
#[test]
fn a_quoted_marker_draws_no_player() {
    let markers = editor("fn trailing_markers(", "pub fn split_audio_marker(");
    assert!(
        markers.contains("pub fn marker_paths(text: &str) -> Vec<PathBuf>")
            && markers.contains("for span in attachment_spans(text)"),
        "the paths a row draws come from the line tails, never from the whole body"
    );
    assert!(
        markers.contains("for line in text.split_inclusive('\\n')"),
        "the body is read line by line, so a joined message keeps its own tail"
    );
    assert!(
        markers.contains("head.rfind(\"<<\")"),
        "the tail is read from the end, so a run of markers needs no count"
    );
}
