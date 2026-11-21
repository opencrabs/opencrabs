//! Wiring for the instance strip: where the credentials are read, where a pick
//! lands, and the health probe loop.
//!
//! Kept out of `dashboard_rail.rs` because it answers a different question. The
//! rail lists the sessions of *this* crab; the strip decides which crab that is,
//! so the two only meet at the render pass, and mixing their state would make
//! "the rail is closed" and "the remote is offline" share a file.
//!
//! Two rules here:
//!
//! - **The disk is read on the UI thread exactly once**, when the window opens
//!   or after a pairing. `state/remote/instances.json` is a small file, but a
//!   read on every frame is how a rail refresh turns into a stall.
//! - **Probes run on the background executor and only the result comes back.**
//!   A remote that accepts the connection and then never answers must not be
//!   able to freeze a window, so every probe carries
//!   [`instance_client::PROBE_TIMEOUT`].

use super::*;
use crate::instance_strip::{self, StripFrame};
use crate::instances::{GLYPH_MAX_CHARS, Health, Instances, glyph_is_picture, normalise_glyph};
use crate::menu;
use crate::remote_panel::{self, RemotePanelFrame};
use crate::topic_icons;
use crate::transcript;
use gpui::ScrollHandle;
use opencrabs::config::Config;
use opencrabs::remote::instance_client::{self, Target};
use opencrabs::remote::instance_store::{InstanceStore, KnownInstance, now};
use opencrabs::remote::pairing::{CODE_TTL_SECS, PairingStore};
use opencrabs::remote::tunnel;

/// How often the probe loop wakes. Five seconds is the same cadence the
/// channel chip already pays, and it is the only clock the strip has: the
/// paint never asks for a probe, it only draws what the last one recorded.
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Run one remote call on the shared runtime.
///
/// `reqwest` panics with *"there is no reactor running, must be called from the
/// context of a Tokio 1.x runtime"* when it is built outside one, and gpui's
/// background executor is not a Tokio runtime. It only looked fine because
/// nothing had paired yet: the panic fires on the first `emparelhar`, and it
/// took the window down with it (2026-11-20 23:46Z).
///
/// This used to build a fresh current-thread runtime per call, and a fresh
/// `reqwest::Client` with it. Both were paid on every probe of every crab every
/// five seconds, and the client again on every fetch, which is why the strip
/// felt slow before any network latency was involved. The runtime now lives in
/// `remote::instance_client` and the client is the process-wide one, so the
/// cost is paid once (measured 21 Nov, P4 in `perf-switch.md`).
///
/// The call still runs off the UI thread: it is the background executor that
/// awaits this future, and `block_on` only drives it to completion.
fn on_remote_runtime<T, F>(call: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    instance_client::block_on(call)
}

impl Dashboard {
    /// Read `state/remote/instances.json` into the window's model.
    ///
    /// A missing file is an empty strip, not an error: a fresh install has no
    /// remotes and must not open with a red banner. A corrupt file is an
    /// error, and it is said out loud, because silently showing an empty strip
    /// when the owner paired three crabs is how a bug hides for a week.
    pub(crate) fn load_instances(&mut self, cx: &mut Context<Self>) {
        let store = InstanceStore::default_path();
        let store = InstanceStore::new(store);
        match store.load() {
            Ok(file) => {
                self.instances = Instances::from_file(&file);
                self.instance_error = None;
            }
            Err(e) => {
                logging::info(&format!("instances: could not read the store: {e:#}"));
                self.instances = Instances::default();
                self.instance_error = Some(SharedString::from(format!(
                    "instâncias: não consegui ler o ficheiro ({e})"
                )));
            }
        }
        cx.notify();
    }

    /// A badge was clicked. Clicking the badge you are already on goes back to
    /// this window's own crab: the strip is a picker, and a picker whose only
    /// exit is the same picker is a trap.
    pub(crate) fn instance_picked(&mut self, host_id: &str, cx: &mut Context<Self>) {
        if self.instances.selected_host_id() == Some(host_id) {
            self.instance_clear(cx);
            return;
        }
        self.instance_open(host_id, cx);
    }

    /// Open a crab whether or not it is already the selection.
    ///
    /// The menu's "abrir" means open, not toggle: picking the crab you are
    /// already reading from a menu must not close it, which is the one thing a
    /// badge click does differently.
    pub(crate) fn instance_open(&mut self, host_id: &str, cx: &mut Context<Self>) {
        if !self.instances.select(host_id) {
            return;
        }
        self.remote_transcript = None;
        self.instance_fetch_remote(host_id.to_string(), cx);
        cx.notify();
    }

    /// Back to the local crab: the workspace mark at the top of the strip.
    pub(crate) fn instance_clear(&mut self, cx: &mut Context<Self>) {
        self.instances.clear_selection();
        self.instance_remote = None;
        self.remote_transcript = None;
        // Half-typed words belong to the crab they were typed for. Keeping
        // them would let Enter, pressed after coming back and opening another
        // crab, send one crab's draft to a different machine.
        self.remote_draft = text_field::TextField::new();
        cx.notify();
    }

    /// Close the remote view and keep the pairing.
    ///
    /// The panel's "desligar" is the one control a user reaches for when the
    /// other machine is off or slow, and it used to delete the stored crab: the
    /// next look needed a new code, minted on the far side, from a crab that
    /// might be the reason it did not answer. Closing the view and forgetting
    /// the crab are two different acts, so they are two different controls.
    pub(crate) fn instance_disconnect(&mut self, cx: &mut Context<Self>) {
        self.instance_remote = None;
        self.remote_transcript = None;
        // Closing the view drops the draft with it, for the same reason
        // `instance_clear` does: the words were addressed to that crab.
        self.remote_draft = text_field::TextField::new();
        cx.notify();
    }

    /// Open the badge's mark editor for one crab.
    pub(crate) fn instance_glyph_begin(
        &mut self,
        host_id: &str,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .instances
            .get(host_id)
            .and_then(|e| e.known.glyph.clone());
        self.instance_glyph = Some(GlyphForm::new(
            host_id.to_string(),
            label.to_string(),
            current,
        ));
        cx.notify();
    }

    /// Write the mark into the store, then into the strip.
    ///
    /// An empty field is not an error: it means "derive it again", and the
    /// record goes back to `None` so the initial follows the label the way it
    /// did before anyone typed anything.
    pub(crate) fn instance_glyph_submit(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.instance_glyph.clone() else {
            return;
        };
        let glyph = normalise_glyph(form.field.text());
        self.instance_glyph_write(&form.host_id, glyph, cx);
    }

    /// Apply one picture from the grid as the crab's mark.
    ///
    /// A pick is the whole act, the way it is in the topic prompt: the owner
    /// clicks a picture and the badge wears it. A grid that only moved the
    /// preview and waited for a second click is the two-step that made this
    /// feel unlike the topics (#3133).
    pub(crate) fn instance_glyph_pick(&mut self, stored: String, cx: &mut Context<Self>) {
        let Some(form) = self.instance_glyph.clone() else {
            return;
        };
        self.instance_glyph_write(&form.host_id, Some(stored), cx);
    }

    /// The one write both paths take: store first, then the strip's model, and
    /// the modal only closes once the disk took it.
    fn instance_glyph_write(
        &mut self,
        host_id: &str,
        glyph: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let store = InstanceStore::new(InstanceStore::default_path());
        match store.find(host_id) {
            Ok(Some(mut known)) => {
                known.glyph = glyph;
                if let Err(e) = store.upsert(known.clone()) {
                    self.instance_error = Some(SharedString::from(format!("ícone: {e:#}")));
                    cx.notify();
                    return;
                }
                self.instances.upsert(known);
                self.instance_error = None;
                self.instance_glyph = None;
            }
            Ok(None) => {
                // The crab was forgotten from another window while the editor
                // was open. Saying so beats writing a mark nobody will see.
                self.instance_error = Some(SharedString::from("esse crab já não está na lista"));
            }
            Err(e) => {
                self.instance_error = Some(SharedString::from(format!("ícone: {e:#}")));
            }
        }
        cx.notify();
    }

    /// The mark editor's keyboard: Esc closes, Enter saves, everything else is
    /// text. No Tab, because there is one field and nothing to walk to.
    pub(crate) fn glyph_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        match ks.key.as_str() {
            "escape" => {
                self.instance_glyph = None;
                self.instance_error = None;
                cx.notify();
            }
            "enter" => self.instance_glyph_submit(cx),
            _ => {
                if let Some(edit) = text_field::editing_chord(
                    ks.key.as_str(),
                    ks.modifiers.control,
                    ks.modifiers.platform,
                    ks.modifiers.alt,
                    ks.modifiers.shift,
                ) {
                    self.apply_glyph_edit(edit, cx);
                    return true;
                }
                // `key_char` and not `key`: the latter is the key's name, so a
                // space would have inserted an "s". A space is whitespace and
                // the mark never keeps one, so it is dropped whole.
                let typed = if ks.key == "space" {
                    None
                } else {
                    ks.key_char.as_ref().and_then(|s| s.chars().next())
                };
                if let Some(ch) = typed.filter(|c| !c.is_control()) {
                    // A picture mark is a stored name, not characters. Typing
                    // over one starts a fresh mark instead of appending to the
                    // name, which would store a string that is neither a mark
                    // nor a picture.
                    let starts_fresh = self
                        .instance_glyph
                        .as_ref()
                        .is_some_and(|f| glyph_is_picture(f.field.text()));
                    if starts_fresh && let Some(form) = self.instance_glyph.as_mut() {
                        form.field = text_field::TextField::new();
                    }
                    // The cap is the field's, so a third character is simply
                    // not accepted: no message, nothing typed that will be cut.
                    let room = self
                        .instance_glyph
                        .as_ref()
                        .is_some_and(|f| f.field.text().chars().count() < GLYPH_MAX_CHARS);
                    if room && let Some(form) = self.instance_glyph.as_mut() {
                        form.field.insert_text(&ch.to_string());
                        cx.notify();
                    }
                }
            }
        }
        true
    }

    /// One editing chord in the mark field, clipboard included.
    fn apply_glyph_edit(&mut self, edit: text_field::TextFieldEdit, cx: &mut Context<Self>) {
        let Some(form) = self.instance_glyph.as_mut() else {
            return;
        };
        // The field can hold a whole picture name (`topic:crab_cli`), fourteen
        // characters in a field capped at two. Typing over one starts fresh
        // (see `glyph_key`); an editing chord that writes has to do the same,
        // or one backspace leaves `topic:crab_cl`, a name that resolves to no
        // icon and would be stored and painted raw. Movement, select-all and
        // copy keep the name on screen because none of them write.
        let writes = matches!(
            edit,
            text_field::TextFieldEdit::Backspace { .. }
                | text_field::TextFieldEdit::Delete { .. }
                | text_field::TextFieldEdit::DeleteToLineStart
                | text_field::TextFieldEdit::Cut
                | text_field::TextFieldEdit::Paste
        );
        if writes && glyph_is_picture(form.field.text()) {
            form.field = text_field::TextField::new();
        }
        match text_field::apply(&mut form.field, edit) {
            Some(text_field::ClipboardRequest::Write(text)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Some(text_field::ClipboardRequest::Read) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    let cut: String = text
                        .trim()
                        .chars()
                        .filter(|c| !c.is_whitespace() && !c.is_control())
                        .take(GLYPH_MAX_CHARS)
                        .collect();
                    form.field.insert_text(&cut);
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// Whether the window is showing a remote crab rather than its own.
    pub(crate) fn instance_remote_open(&self) -> bool {
        self.instance_remote.is_some()
    }

    /// The selected crab's sessions as rail rows.
    ///
    /// `None` means "not looking at a remote", which leaves the local rail
    /// alone. `Some(rows)` may be empty, and an empty rail beside a crab that
    /// answered is the honest picture of a crab with no sessions.
    pub(crate) fn remote_rail_rows(&self) -> Option<Vec<rail_view::RailRow>> {
        let remote = self.instance_remote.as_ref()?;
        let sessions = match &remote.state {
            RemoteState::Ready(sessions) => sessions,
            // Loading and failed both leave the rail empty: a stale list beside
            // a crab that just refused the call would be a lie the paint cannot
            // take back.
            _ => return Some(Vec::new()),
        };
        let selected = self
            .remote_transcript
            .as_ref()
            .map(|t| t.session_id.as_str());
        Some(
            sessions
                .iter()
                .filter_map(|s| {
                    let id = uuid::Uuid::parse_str(&s.id).ok()?;
                    Some(rail_view::RailRow {
                        card: sessions::RailCard {
                            id,
                            title: Some(s.title.clone()).filter(|t| !t.is_empty()),
                        },
                        favorite: false,
                        flags: rail_view::RailRowFlags {
                            selected: selected == Some(s.id.as_str()),
                            cursored: false,
                            indented: false,
                            closed: false,
                            mark: rail_activity::RowState::default(),
                            preview: s.model.clone().map(SharedString::from),
                            activity: None,
                            plan: None,
                            flash: false,
                            running: None,
                            cost: 0.0,
                            // The remote speaks unix seconds, the rail paints
                            // `DateTime<Utc>`, and a stamp of zero is a session
                            // the remote never dated: epoch, not a panic.
                            stamp_at: chrono::DateTime::from_timestamp(s.updated_at, 0)
                                .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
                            icon: None,
                        },
                        chips: None,
                    })
                })
                .collect(),
        )
    }

    /// Open one of the remote's sessions: fetch its transcript.
    pub(crate) fn open_remote_session(
        &mut self,
        session_id: String,
        title: String,
        cx: &mut Context<Self>,
    ) {
        let Some(remote) = self.instance_remote.as_ref() else {
            return;
        };
        let url = remote.url.clone();
        let host_id = remote.host_id.clone();
        let Some(entry) = self.instances.get(&host_id) else {
            return;
        };
        let token = entry.known.token.clone();
        self.remote_transcript = Some(RemoteTranscript {
            session_id: session_id.clone(),
            context_id: None,
            title: SharedString::from(title),
            state: Default::default(),
            has_rows: false,
            notice: None,
            sending: false,
            scroll: ScrollHandle::new(),
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            let fetch_url = url.clone();
            let fetch_id = session_id.clone();
            // The turns are built here, on the background executor, not inside
            // `this.update` below. Building them parses markdown for every row
            // of the session, and a 216 KB session is tens of thousands of
            // blocks: doing that on the UI thread froze the window for the
            // whole parse, which is what "the switch takes ages" actually was
            // (measured 21 Nov, P4 in perf-switch.md). `TranscriptState` is
            // plain data with no `Rc` in it, so it crosses the executor
            // boundary intact.
            let build_id = session_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let read = on_remote_runtime(async {
                        let target = Target::new(&fetch_url, Some(&token));
                        instance_client::transcript(&target, &fetch_id, 40).await
                    })?;
                    let state = remote_panel::state_for(&build_id, &read.messages);
                    let has_rows = state.turns.iter().any(|t| !t.blocks.is_empty());
                    // The context rides with the rows: a send that does not
                    // carry it opens a new session on the far side, and the
                    // reply lands in a conversation this panel is not drawing
                    // (#3133).
                    Ok::<_, anyhow::Error>((read.title, read.context_id, state, has_rows))
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                let still_here = this
                    .remote_transcript
                    .as_ref()
                    .is_some_and(|t| t.session_id == session_id);
                if !still_here {
                    return;
                }
                match result {
                    Ok((title, context_id, state, has_rows)) => {
                        if let Some(transcript) = this.remote_transcript.as_mut() {
                            transcript.context_id = context_id;
                            // The host's title wins over the rail's: the list
                            // may be a minute old, and a rename in between is
                            // exactly what the banner would get wrong.
                            if !title.is_empty() {
                                transcript.title = SharedString::from(title);
                            }
                            // The whole remote session went through the
                            // transcript's own rules on the executor (see the
                            // spawn above): a seed and a compaction marker are
                            // not conversation, reasoning folds, a 40 KB answer
                            // is capped. This only installs the finished state,
                            // so the UI thread does no parsing at all.
                            transcript.state = state;
                            transcript.has_rows = has_rows;
                            transcript.notice = None;
                        }
                    }
                    Err(e) => {
                        if let Some(transcript) = this.remote_transcript.as_mut() {
                            transcript.notice =
                                Some(SharedString::from(format!("não consegui ler: {e:#}")));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The remote composer's keyboard. Returns whether the key was consumed.
    ///
    /// While a remote crab is open the only composer on screen belongs to it,
    /// so every printable character, every editing chord and Enter must land
    /// here rather than in the local editor, which is not drawn and whose
    /// contents would be sent to this window's own agent if it were.
    pub(crate) fn remote_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        match ks.key.as_str() {
            "enter" => {
                self.send_remote(cx);
                true
            }
            "escape" => {
                // Esc leaves the crab and goes back to this window's own rail.
                self.instance_clear(cx);
                true
            }
            _ => {
                if let Some(edit) = text_field::editing_chord(
                    &ks.key,
                    ks.modifiers.control,
                    ks.modifiers.platform,
                    ks.modifiers.alt,
                    ks.modifiers.shift,
                ) {
                    self.apply_remote_edit(edit, cx);
                    return true;
                }
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    // A chord this field does not implement belongs to nobody
                    // here: swallowing it would eat a window shortcut, and
                    // letting it through would type into an unseen editor.
                    return true;
                }
                let typed = if ks.key == "space" {
                    Some(' ')
                } else {
                    ks.key_char.as_ref().and_then(|s| s.chars().next())
                };
                match typed {
                    Some(ch) => {
                        if self.remote_draft.type_char(ch) {
                            cx.notify();
                        }
                        true
                    }
                    // Arrows, page keys, function keys: the field has no line
                    // to walk, so they change nothing and stop here.
                    None => true,
                }
            }
        }
    }

    /// Apply one editing chord to the remote composer, clipboard included.
    fn apply_remote_edit(&mut self, edit: text_field::TextFieldEdit, cx: &mut Context<Self>) {
        match text_field::apply(&mut self.remote_draft, edit) {
            Some(text_field::ClipboardRequest::Write(text)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Some(text_field::ClipboardRequest::Read) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.remote_draft.insert_text(&text);
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// The pairing form's keyboard: three fields, Tab between them, Enter to
    /// submit, Esc to give up. Returns whether the key was consumed.
    pub(crate) fn pair_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(busy) = self.instance_pairing.as_ref().map(|form| form.busy) else {
            return false;
        };
        if busy {
            // A submit in flight owns nothing: the answer comes back in the
            // form itself, and typing into a request already sent would lie.
            return true;
        }
        match ks.key.as_str() {
            "escape" => {
                self.instance_pairing = None;
                self.instance_error = None;
                cx.notify();
            }
            "enter" => self.instance_pair_submit(cx),
            "tab" => {
                if let Some(form) = self.instance_pairing.as_mut() {
                    let back = ks.modifiers.shift;
                    form.field = if back {
                        (form.field + 2) % 3
                    } else {
                        (form.field + 1) % 3
                    };
                }
                cx.notify();
            }
            _ => {
                if let Some(edit) = text_field::editing_chord(
                    ks.key.as_str(),
                    ks.modifiers.control,
                    ks.modifiers.platform,
                    ks.modifiers.alt,
                    ks.modifiers.shift,
                ) {
                    self.apply_pair_edit(edit, cx);
                    return true;
                }
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    // A chord this field does not implement belongs to nobody
                    // here: swallowing it would eat a window shortcut.
                    return true;
                }
                let typed = if ks.key == "space" {
                    Some(' ')
                } else {
                    ks.key_char.as_ref().and_then(|s| s.chars().next())
                };
                if let Some(ch) = typed {
                    if let Some(form) = self.instance_pairing.as_mut() {
                        form.focused().insert_text(&ch.to_string());
                    }
                    cx.notify();
                }
            }
        }
        true
    }

    /// Apply one editing chord to the focused pairing field, clipboard
    /// included, exactly as the composer does.
    fn apply_pair_edit(&mut self, edit: text_field::TextFieldEdit, cx: &mut Context<Self>) {
        let Some(form) = self.instance_pairing.as_mut() else {
            return;
        };
        let in_url = form.field == 0;
        match text_field::apply(form.focused(), edit) {
            Some(text_field::ClipboardRequest::Write(text)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Some(text_field::ClipboardRequest::Read) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    // A pasted pairing link fills both fields at once, which is
                    // the whole reason the other crab prints one.
                    if in_url && form.absorb_link(&text) {
                        cx.notify();
                        return;
                    }
                    form.focused().insert_text(&text);
                }
            }
            None => {}
        }
        cx.notify();
    }

    /// The pairing modal: the URL the other crab announces, the code it printed
    /// and a name for the strip. Nothing touches the network until Enter.
    pub(crate) fn instance_pair_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.instance_pairing.clone() else {
            return div().into_any_element();
        };
        let shown = palette::current();
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, (label, field)) in form.fields().iter().enumerate() {
            let here = form.field == index;
            // The same span split the composer and the topic prompt use: the
            // window never slices a string, the core does.
            let (before, selected, after) = field.render_spans();
            // A field that only Tab can reach is a field half the owners never
            // find: the form printed three boxes that looked editable and
            // ignored the mouse entirely (2026-11-21, Daniel: "no ecra de
            // adicionar crab nao consigo carregar com o rato noutros inputs").
            // Clicking a row now moves the caret there, exactly like Tab.
            let focus_here = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                if let Some(form) = this.instance_pairing.as_mut() {
                    form.field = index;
                }
                cx.notify();
            });
            body.push(
                div()
                    .id(SharedString::from(format!("instance-pair-field-{index}")))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(focus_here)
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .text_color(rgb(if here {
                                shown.text_normal
                            } else {
                                shown.text_faint
                            }))
                            .child(SharedString::from(*label)),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(shown.panel))
                            .border_1()
                            .border_color(rgb(if here { shown.accent } else { shown.track }))
                            .text_size(rems(0.8125))
                            .text_color(rgb(if field.is_empty() {
                                shown.text_faint
                            } else {
                                shown.text
                            }))
                            // A field with nothing in it and no caret on it is
                            // a hole in the form, so it says what it wants.
                            .when(field.is_empty() && !here, |d| {
                                d.child(SharedString::from(match index {
                                    0 => "https://…",
                                    1 => "8 caracteres",
                                    _ => "o nome do outro crab",
                                }))
                            })
                            .child(SharedString::from(before.to_string()))
                            .when(!selected.is_empty(), |d| {
                                d.child(
                                    div()
                                        .bg(rgb(shown.selection))
                                        .child(SharedString::from(selected.to_string())),
                                )
                            })
                            .when(here, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .w(px(1.5))
                                        .h(rems(1.0))
                                        .bg(rgb(shown.accent)),
                                )
                            })
                            .child(SharedString::from(after.to_string())),
                    )
                    .into_any_element(),
            );
        }
        if let Some(err) = &self.instance_error {
            body.push(
                div()
                    .text_size(rems(0.75))
                    .text_color(rgb(shown.warn))
                    .child(err.clone())
                    .into_any_element(),
            );
        }
        let submit = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_pair_submit(cx);
        });
        body.push(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .when(form.busy, |d| d.opacity(0.6))
                .child(
                    div()
                        .id("instance-pair-submit")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .when(!form.busy, |d| d.cursor_pointer())
                        .bg(rgb(shown.accent))
                        .text_size(rems(0.75))
                        .text_color(rgb(shown.bg))
                        .child(SharedString::from(if form.busy {
                            "a emparelhar…"
                        } else {
                            "emparelhar"
                        }))
                        .on_click(submit),
                )
                .into_any_element(),
        );

        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_pairing = None;
            this.instance_error = None;
            cx.notify();
        });
        modal_shell::render(
            modal_shell::ModalView {
                id: "instance-pair-modal",
                title: "Emparelhar um crab".to_string(),
                subtitle: "o outro crab mostra o URL e um código de uso único".to_string(),
                actions: None,
                header_close: true,
                body: div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(body)
                    .into_any_element(),
                footer: Some(
                    modal_shell::footer("tab campos · enter emparelha · esc fecha".to_string())
                        .into_any_element(),
                ),
                width: 26.0 * self.rem,
                max_height: 26.0 * self.rem,
            },
            dismiss,
        )
        .into_any_element()
    }

    /// The badge's mark editor: one field, one save.
    ///
    /// A modal and not a menu of preset letters: the whole point is that the
    /// owner picks the mark, and "the first letter of my other crab's name" is
    /// exactly the collision this exists to fix.
    pub(crate) fn instance_glyph_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.instance_glyph.clone() else {
            return div().into_any_element();
        };
        let shown = palette::current();
        let (before, selected, after) = form.field.render_spans();
        let typed = form.field.text().chars().count();
        // The badge previews what the field holds: a picture when the mark is
        // one, the characters otherwise.
        let preview: AnyElement = match normalise_glyph(form.field.text()) {
            Some(mark) => match topic_icons::tab_icon(&mark, 1.5) {
                Some(picture) => picture,
                None => div()
                    .text_size(rems(0.875))
                    .text_color(rgb(shown.text_title))
                    .child(SharedString::from(mark))
                    .into_any_element(),
            },
            None => div()
                .text_size(rems(0.875))
                .text_color(rgb(shown.text_title))
                .child(SharedString::from("?"))
                .into_any_element(),
        };
        // The same pack the topic prompt shows (#3133): a crab's badge and a
        // topic's tab wear one set, so picking here is picking there. A pick
        // applies at once, which is what "igual aos topics" means.
        let families: Vec<(&'static str, Vec<Stateful<Div>>)> = topic_icons::TOPIC_FAMILIES
            .iter()
            .map(|&(caption, prefix)| {
                let cells: Vec<Stateful<Div>> = topic_icons::family_icons(prefix)
                    .map(|(name, bytes)| {
                        let pick = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                            this.instance_glyph_pick(topic_icons::store_name(name), cx);
                        });
                        div()
                            .id(SharedString::from(format!("instance-glyph-icon-{name}")))
                            .p_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                            .on_click(pick)
                            .child(topic_icons::picture(bytes, 3.0))
                    })
                    .collect();
                (caption, cells)
            })
            .collect();
        let submit = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_glyph_submit(cx);
        });
        let dismiss = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_glyph = None;
            this.instance_error = None;
            cx.notify();
        });
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(shown.text_faint))
                    .child(SharedString::from(format!(
                        "o ícone de {} na coluna da esquerda: uma letra (até {GLYPH_MAX_CHARS} caracteres, vazio volta à inicial) ou uma imagem abaixo",
                        form.label
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .w(rems(2.0))
                            .h(rems(2.0))
                            .rounded_md()
                            .bg(rgb(shown.panel_hi))
                            .border_1()
                            .border_color(rgb(shown.accent))
                            .child(preview),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(shown.panel))
                            .border_1()
                            .border_color(rgb(shown.accent))
                            .text_size(rems(0.8125))
                            .text_color(rgb(shown.text))
                            .when(form.field.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_color(rgb(shown.text_faint))
                                        .child(SharedString::from("ex.: L, 7, 🇵🇹")),
                                )
                            })
                            .child(SharedString::from(before.to_string()))
                            .when(!selected.is_empty(), |d| {
                                d.child(
                                    div()
                                        .bg(rgb(shown.selection))
                                        .child(SharedString::from(selected.to_string())),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(1.5))
                                    .h(rems(1.0))
                                    .bg(rgb(shown.accent)),
                            )
                            .child(SharedString::from(after.to_string())),
                    ),
            )
            // The pack, the same one the topic prompt shows (#3133). A pick
            // applies at once: the modal closes with the mark written, which
            // is what picking an icon in the topic prompt does.
            .child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(shown.text_faint))
                    .child(SharedString::from("ou escolhe uma imagem")),
            )
            .child(
                div()
                    .id("instance-glyph-grid")
                    .max_h(rems(11.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(families.into_iter().map(|(caption, cells)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(rems(0.625))
                                    .text_color(rgb(shown.text_faint))
                                    .child(SharedString::from(caption)),
                            )
                            .child(div().flex().flex_row().flex_wrap().gap_1().children(cells))
                    })),
            )
            .when(typed >= GLYPH_MAX_CHARS, |d| {
                d.child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from("cheio: apaga uma letra para trocar")),
                )
            })
            .when_some(self.instance_error.clone(), |d, err| {
                d.child(
                    div()
                        .text_size(rems(0.75))
                        .text_color(rgb(shown.warn))
                        .child(err),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("instance-glyph-save")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(rgb(shown.accent))
                            .text_size(rems(0.75))
                            .text_color(rgb(shown.bg))
                            .child(SharedString::from("guardar"))
                            .on_click(submit),
                    )
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .text_color(rgb(shown.text_faint))
                            .child(SharedString::from(
                                "enter guarda · esc fecha · vazio volta à inicial",
                            )),
                    ),
            )
            .into_any_element();
        modal_shell::render(
            modal_shell::ModalView {
                id: "instance-glyph-modal",
                title: "Ícone do badge".to_string(),
                subtitle: form.label.clone(),
                actions: None,
                header_close: true,
                body,
                footer: None,
                width: 22.0 * self.rem,
                // Tall enough for the two families the pack ships and the
                // field above them; the grid scrolls past that (#3133).
                max_height: 30.0 * self.rem,
            },
            dismiss,
        )
        .into_any_element()
    }

    /// The panel column: the banner, the transcript and the remote composer.
    pub(crate) fn remote_panel_element(
        &mut self,
        measure: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(remote) = self.instance_remote.as_ref() else {
            return div().into_any_element();
        };
        let entry = self.instances.get(&remote.host_id);
        let transcript = self.remote_transcript.as_ref();
        let notice = match (&remote.state, transcript) {
            (RemoteState::Loading, _) => Some(SharedString::from("a falar com o crab…")),
            (RemoteState::Failed(e), _) => Some(SharedString::from(e.clone())),
            (_, Some(t)) => t.notice.clone(),
            (_, None) => Some(SharedString::from(
                "escolhe uma sessão na rail para ler a conversa",
            )),
        };
        let frame = RemotePanelFrame {
            // `label()` borrows the entry; `SharedString` only takes `'static`, so
            // the copy happens here and the frame owns its own bytes.
            label: match entry {
                Some(e) => SharedString::from(e.label().to_string()),
                None => SharedString::from("crab"),
            },
            url: SharedString::from(remote.url.clone()),
            health: entry.map(|e| e.health),
            session_title: transcript.map(|t| t.title.clone()),
            has_rows: transcript.is_some_and(|t| t.has_rows),
            notice,
            sending: transcript.is_some_and(|t| t.sending),
            copied: self.copied_row(),
        };
        // One fold click per turn anchor, minted here because only this `cx`
        // can build one and only this view owns the state the click mutates.
        let mut handlers: std::collections::HashMap<String, transcript::ClickHandler> =
            std::collections::HashMap::new();
        if let Some(t) = self.remote_transcript.as_ref() {
            for anchor in t.state.anchors() {
                let key = anchor.clone();
                handlers.insert(
                    anchor,
                    std::rc::Rc::new(cx.listener(move |this, _ev, _window, cx| {
                        if let Some(t) = this.remote_transcript.as_mut() {
                            t.state.toggle_turn(&key);
                        }
                        cx.notify();
                    })),
                );
            }
        }
        // One copy handler per answer, minted here for the same reason the fold
        // clicks are: the clipboard and the "copied" tick belong to this window.
        // `msg_copy_markdown` copies the string it is handed and never reads the
        // row back out of SQLite, so it works on a message this database has
        // never stored, which every remote row is.
        let mut copy: std::collections::HashMap<uuid::Uuid, transcript::ClickHandler> =
            std::collections::HashMap::new();
        if let Some(t) = self.remote_transcript.as_ref() {
            for row in t.state.rows.iter().filter(|r| r.role == "assistant") {
                let id = row.id;
                let content = row.content.clone();
                copy.insert(
                    id,
                    std::rc::Rc::new(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        this.msg_copy_markdown(id, content.clone(), cx);
                    })),
                );
            }
        }
        let send = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.send_remote(cx);
        });
        // "desligar" closes the view; it does not touch the store. Forgetting a
        // crab is its badge's right-click menu, because that costs the pairing
        // and a fresh code is a round trip to the other machine.
        let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_disconnect(cx);
        });
        // Borrowed after the frame is built: `state` is not `Clone`, and cloning
        // 200 KB of rows per paint to satisfy a frame field would be a worse bug
        // than the divergence this panel is fixing. `None` when no session is
        // open, which is exactly when there is no body to paint.
        let body = self
            .remote_transcript
            .as_ref()
            .map(|t| (&t.state, &t.scroll));
        remote_panel::view(
            &frame,
            body,
            measure,
            self.rem,
            handlers,
            copy,
            &self.remote_draft,
            move |ev, window, app| send(ev, window, app),
            move |ev, window, app| close(ev, window, app),
        )
    }

    /// Send the composer's text to the selected crab as a task.
    ///
    /// This is `message/send`, the same call the A2A tool makes, so a remote
    /// crab treats it exactly as any other peer's task. The task id comes back
    /// and is dropped into the notice: this panel reads, it does not follow the
    /// task's progress, and saying so is better than pretending otherwise.
    pub(crate) fn send_remote(&mut self, cx: &mut Context<Self>) {
        let text = self.remote_draft.trimmed().to_string();
        if text.is_empty() {
            return;
        }
        let Some(remote) = self.instance_remote.as_ref() else {
            return;
        };
        let url = remote.url.clone();
        let host_id = remote.host_id.clone();
        let Some(entry) = self.instances.get(&host_id) else {
            return;
        };
        let token = entry.known.token.clone();
        // One send at a time. The field empties the moment the words leave, so
        // a second Enter would post nothing, but a second *click* on "enviar"
        // could still fire on the frame before `sending` paints.
        if self.remote_transcript.as_ref().is_some_and(|t| t.sending) {
            return;
        }
        // The conversation this send belongs to. A transcript read carries the
        // host's context; a send without it makes the host open a new session,
        // which is what made the composer look like it sent nothing at all
        // (#3133).
        let context_id = self
            .remote_transcript
            .as_ref()
            .and_then(|t| t.context_id.clone());
        let session_id = self
            .remote_transcript
            .as_ref()
            .map(|t| t.session_id.clone());
        if let Some(transcript) = self.remote_transcript.as_mut() {
            transcript.sending = true;
        }
        self.remote_draft = text_field::TextField::new();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let send_url = url.clone();
            let body = text.clone();
            let ctx = context_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    on_remote_runtime(async {
                        let target = Target::new(&send_url, Some(&token));
                        instance_client::send(&target, &body, ctx.as_deref()).await
                    })
                })
                .await;
            let sent = result.is_ok();

            let _ = this.update(cx, |this, cx| {
                if let Some(transcript) = this.remote_transcript.as_mut() {
                    transcript.sending = false;
                    match &result {
                        Ok(task) => {
                            // Adopt the host's context. The first send to a
                            // session that never spoke A2A opens one, and the
                            // next send has to reuse it or fork again.
                            if transcript.context_id.is_none() {
                                transcript.context_id = task.context_id.clone();
                            }
                            transcript.notice =
                                Some(SharedString::from(format!("tarefa criada: {}", task.id)));
                        }
                        Err(e) => {
                            transcript.notice =
                                Some(SharedString::from(format!("não enviou: {e:#}")));
                        }
                    }
                }
                if result.is_err() {
                    // Give the words back: losing a typed message to a network
                    // error is the one thing a composer must never do.
                    this.remote_draft = text_field::TextField::from_text(&text);
                }
                cx.notify();
            });

            if !sent {
                return;
            }
            // The host writes the user's row when it starts the task, a beat
            // after it answers. One short wait and one re-read, so the words
            // appear in the conversation instead of only in a notice line.
            let Some(session_id) = session_id else {
                return;
            };
            cx.background_executor()
                .timer(std::time::Duration::from_millis(700))
                .await;
            let _ = this.update(cx, |this, cx| {
                let still_here = this
                    .remote_transcript
                    .as_ref()
                    .is_some_and(|t| t.session_id == session_id);
                if !still_here {
                    return;
                }
                // Re-reading rebuilds the panel, so the context has to survive
                // the rebuild: the fetch fills it from the host a moment later,
                // and losing it in between would fork the very next send.
                let keep = this
                    .remote_transcript
                    .as_ref()
                    .and_then(|t| t.context_id.clone());
                let title = this
                    .remote_transcript
                    .as_ref()
                    .map(|t| t.title.to_string())
                    .unwrap_or_default();
                this.open_remote_session(session_id.clone(), title, cx);
                if let Some(transcript) = this.remote_transcript.as_mut() {
                    transcript.context_id = keep;
                }
            });
        })
        .detach();
    }

    /// The `+` chip was clicked. It opens the pairing form; it does not touch
    /// the network until the form is submitted with a URL and a code.
    pub(crate) fn instance_add_clicked(&mut self, cx: &mut Context<Self>) {
        self.instance_pairing = Some(PairingForm::default());
        cx.notify();
    }

    /// The strip as a cached view, handed one frame of plain data.
    pub(crate) fn instance_strip_element(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let frame = StripFrame::from_model(&self.instances, now(), self.instance_pairing.is_some());
        let view = self.instance_strip.clone();
        view.update(cx, |strip, cx| strip.set_frame(frame, cx));
        AnyView::from(view)
            .cached(instance_strip::strip_root_style())
            .into_any_element()
    }

    /// Start the probe loop. Called once from the window's constructor; the
    /// loop exits when the window dies, like every other loop there.
    pub(crate) fn spawn_instance_probe(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROBE_INTERVAL).await;
                // Snapshot the targets on the UI thread: the model is not
                // shared state and the worker must not read it.
                let targets = match this.update(cx, |this, _cx| {
                    this.instances
                        .entries()
                        .iter()
                        .map(|entry| {
                            (
                                entry.host_id().to_string(),
                                entry.url().to_string(),
                                entry.known.token.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                }) {
                    Ok(list) => list,
                    Err(_) => break,
                };
                if targets.is_empty() {
                    continue;
                }

                let results = match cx
                    .background_executor()
                    .spawn(async move {
                        on_remote_runtime(async move {
                            let probes =
                                targets.into_iter().map(|(host_id, url, token)| async move {
                                    let target = Target::new(&url, Some(&token));
                                    let result = instance_client::probe(&target).await;
                                    (host_id, result)
                                });
                            Ok::<_, anyhow::Error>(futures::future::join_all(probes).await)
                        })
                    })
                    .await
                {
                    Ok(results) => results,
                    Err(e) => {
                        logging::info(&format!("instances: probe round failed: {e:#}"));
                        continue;
                    }
                };

                let at = now();
                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        for (host_id, result) in results {
                            let before = this.instances.get(&host_id).map(|e| e.health);
                            this.instances
                                .record_probe(&host_id, result.map_err(|_| ()), at);
                            let after = this.instances.get(&host_id).map(|e| e.health);
                            changed |= before != after;
                            // A crab that answered is a crab whose token still
                            // works, which is worth remembering across restarts.
                            if matches!(after, Some(Health::Up { .. })) && before != after {
                                let store = InstanceStore::new(InstanceStore::default_path());
                                if let Err(e) = store.record_ok(&host_id, at) {
                                    logging::info(&format!(
                                        "instances: could not stamp {host_id}: {e:#}"
                                    ));
                                }
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Fetch one remote's session list, off the UI thread.
    ///
    /// A failure is stored and shown, never swallowed: "não respondeu" and "não
    /// tem sessões" are different things to tell the owner, and the panel draws
    /// them differently.
    pub(crate) fn instance_fetch_remote(&mut self, host_id: String, cx: &mut Context<Self>) {
        let Some(entry) = self.instances.get(&host_id) else {
            return;
        };
        // Every way into a crab lands here: the badge, the pairing form, and
        // the Ctrl+Alt+Up/Down switch. The draft is per crab, so it is dropped
        // at the one door they all use.
        self.remote_draft = text_field::TextField::new();
        let url = entry.url().to_string();
        let token = entry.known.token.clone();
        self.instance_remote = Some(RemoteView::loading(host_id.clone()));

        cx.spawn(async move |this, cx| {
            let target_url = url.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    on_remote_runtime(async {
                        let target = Target::new(&target_url, Some(&token));
                        instance_client::list_sessions(&target, 50).await
                    })
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                // The pick may have moved on while this was in flight: a result
                // for a crab that is no longer selected is dropped, not painted.
                if this.instances.selected_host_id() != Some(host_id.as_str()) {
                    return;
                }
                this.instance_remote = Some(match result {
                    Ok(sessions) => RemoteView::ready(host_id.clone(), url.clone(), sessions),
                    Err(e) => RemoteView::failed(host_id.clone(), url.clone(), format!("{e:#}")),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Pair a new crab: POST the code, store the token, select it.
    pub(crate) fn instance_pair_submit(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.instance_pairing.clone() else {
            return;
        };
        let url = form.url.trimmed().to_string();
        let code = form.code.trimmed().to_string();
        let label = form.label.trimmed().to_string();
        if url.is_empty() || code.is_empty() {
            self.instance_error = Some(SharedString::from(
                "emparelhar: preciso do URL e do código".to_string(),
            ));
            cx.notify();
            return;
        }
        self.instance_pairing = Some(form.busy());
        self.instance_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let pair_url = url.clone();
            let pair_code = code.clone();
            let pair_label = label.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    on_remote_runtime(async {
                        opencrabs::remote::instance_store::pair(
                            &pair_url,
                            &pair_code,
                            if pair_label.is_empty() {
                                &pair_url
                            } else {
                                &pair_label
                            },
                        )
                        .await
                    })
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(paired) => {
                        let store = InstanceStore::new(InstanceStore::default_path());
                        let known = KnownInstance {
                            host_id: paired.host_id.clone(),
                            label: paired.label.clone(),
                            url: paired.url.clone(),
                            token: paired.token.clone(),
                            added_at: now(),
                            last_ok: 0,
                            glyph: None,
                        };
                        if let Err(e) = store.upsert(known.clone()) {
                            this.instance_error = Some(SharedString::from(format!(
                                "emparelhou, mas não consegui guardar: {e:#}"
                            )));
                        }
                        this.instances.upsert(known);
                        this.instances.select(&paired.host_id);
                        this.instance_pairing = None;
                        this.instance_fetch_remote(paired.host_id.clone(), cx);
                    }
                    Err(e) => {
                        this.instance_error =
                            Some(SharedString::from(format!("emparelhar: {e:#}")));
                        this.instance_pairing = Some(PairingForm {
                            url: TextField::from_text(&url),
                            code: TextField::new(),
                            label: TextField::from_text(&label),
                            busy: false,
                            field: 0,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Remove a crab from the strip and the store.
    pub(crate) fn instance_remove(&mut self, host_id: &str, cx: &mut Context<Self>) {
        let store = InstanceStore::new(InstanceStore::default_path());
        match store.remove(host_id) {
            Ok(true) => {
                self.instances.remove(host_id);
                if self
                    .instance_remote
                    .as_ref()
                    .is_some_and(|r| r.host_id == host_id)
                {
                    self.instance_remote = None;
                }
                self.instance_error = None;
            }
            Ok(false) => {}
            Err(e) => {
                self.instance_error = Some(SharedString::from(format!("remover: {e:#}")));
            }
        }
        cx.notify();
    }

    /// The workspace mark was right-clicked: open its menu at the press point.
    pub(crate) fn instance_mark_menu(
        &mut self,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.instance_menu_at = Some(at);
        self.instance_menu_for = None;
        cx.notify();
    }

    /// A badge was right-clicked: open *its* menu at the press point.
    ///
    /// The same card as the mark's, told apart by `instance_menu_for`. The
    /// badge is the pick, so the destructive row cannot live on it: this menu
    /// is opened on purpose, with a right-click, and that is the confirmation
    /// a left-click would never get.
    pub(crate) fn instance_badge_menu(
        &mut self,
        host_id: &str,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.instance_menu_at = Some(at);
        self.instance_menu_for = Some(host_id.to_string());
        cx.notify();
    }

    /// Whatever the open card is, wired. The mark's menu has three rows,
    /// because the head of the column can hand a code out, take one in, or open
    /// this machine's settings; a badge's has the one row that forgets it.
    pub(crate) fn instance_mark_menu_items(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        if let Some(host_id) = self.instance_menu_for.clone() {
            return self.instance_badge_menu_items(&host_id, cx);
        }
        let share = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_share_begin(cx);
        });
        let pair = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_menu_at = None;
            this.instance_add_clicked(cx);
        });
        let settings = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_menu_at = None;
            this.toggle_settings(cx);
        });
        vec![
            menu::menu_item("gerar código de acesso remoto").on_click(share),
            menu::menu_item("emparelhar um crab").on_click(pair),
            menu::menu_item("definições desta máquina").on_click(settings),
        ]
    }

    /// A badge's menu: open it, or forget it.
    ///
    /// "abrir" is the left-click's job, repeated here because a menu whose only
    /// row is destructive is a trap: the pointer lands on it and a slip of the
    /// hand costs the pairing. Forgetting keeps the name in the row so nobody
    /// deletes the wrong crab.
    fn instance_badge_menu_items(
        &mut self,
        host_id: &str,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let label = self
            .instances
            .get(host_id)
            .map(|e| e.label().to_string())
            .unwrap_or_else(|| host_id.to_string());
        let open_host = host_id.to_string();
        let open = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.instance_menu_at = None;
            this.instance_menu_for = None;
            this.instance_open(&open_host, cx);
        });
        let forget_host = host_id.to_string();
        let forget = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.instance_menu_at = None;
            this.instance_menu_for = None;
            this.instance_remove(&forget_host, cx);
        });
        // The mark, in the middle: the row that exists because two crabs can
        // share an initial, and the strip is where that has to be fixable.
        let mark_host = host_id.to_string();
        let mark_label = label.clone();
        let mark = cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.instance_menu_at = None;
            this.instance_menu_for = None;
            this.instance_glyph_begin(&mark_host, &mark_label, cx);
        });
        vec![
            menu::menu_item("abrir").on_click(open),
            menu::menu_item("mudar ícone").on_click(mark),
            menu::menu_item_owned(SharedString::from(format!("esquecer {label}"))).on_click(forget),
        ]
    }

    /// Mint a code and build the card that carries it.
    ///
    /// The code is minted **in this process**, not by `opencrabs connect`: the
    /// window already runs the A2A gateway (the surface boots it, so it is the
    /// same listener the CLI would tunnel), and `PairingStore` is the same file
    /// that gateway reads. A round trip through the CLI would need a terminal
    /// and cloudflared to say what this says from inside the window.
    pub(crate) fn instance_share_begin(&mut self, cx: &mut Context<Self>) {
        self.instance_menu_at = None;
        self.instance_error = None;
        let store = PairingStore::new(PairingStore::default_path());
        let scopes = vec!["a2a:read".to_string(), "a2a:send".to_string()];
        let code = match store.mint_code(&scopes) {
            Ok(code) => code,
            Err(e) => {
                self.instance_error = Some(SharedString::from(format!("código: {e:#}")));
                cx.notify();
                return;
            }
        };
        // A tunnel that is already running is the truth about how another crab
        // reaches this one, and it outranks the bind address. Measured on the
        // lenovo (21 Nov): cloudflared was publishing 127.0.0.1:18790 on a
        // public hostname while the card told the owner "só aceita ligações
        // locais" and handed out an address nothing outside the box could
        // reach. The card may not contradict the machine it describes.
        let mut live = ShareTunnel::Idle;
        let (url, warning, snippet) = match Config::load() {
            Ok(config) => {
                // `status` reads one small file and checks one pid: no shell,
                // no network, so it rides the same disk read the window does
                // at open instead of needing a background hop.
                let running = tunnel::status(&tunnel::default_state_dir()).ok().flatten();
                if let Some(state) = running.as_ref() {
                    live = ShareTunnel::Up(state.url.clone());
                }
                share_address(&config.a2a, running.as_ref())
            }
            Err(e) => (
                "http://127.0.0.1:18790".to_string(),
                Some(format!("não consegui ler a config: {e}")),
                None,
            ),
        };
        let label = this_host_label();
        let link = pair_link(&url, &code, &label);
        let (qr, width) = qr_modules(&link);
        self.instance_share = Some(ShareView {
            url,
            code,
            link,
            qr,
            width,
            warning,
            snippet,
            tunnel: live,
            ttl_secs: CODE_TTL_SECS,
        });
        cx.notify();
    }

    /// Open a public tunnel and repoint the card at it.
    ///
    /// This is a separate click on purpose. A quick tunnel publishes a URL on
    /// the internet, and doing that behind a button labelled "gerar código"
    /// would be the same class of surprise as minting a code nobody asked for.
    /// The code is untouched: only the address the card hands out changes, and
    /// the link and QR are rebuilt around it.
    pub(crate) fn instance_share_tunnel(&mut self, cx: &mut Context<Self>) {
        if self.instance_share.is_none() {
            return;
        }
        let port = match Config::load() {
            Ok(config) => config.a2a.port,
            Err(e) => {
                if let Some(share) = self.instance_share.as_mut() {
                    share.tunnel = ShareTunnel::Failed(format!("não consegui ler a config: {e}"));
                }
                cx.notify();
                return;
            }
        };
        if let Some(share) = self.instance_share.as_mut() {
            share.tunnel = ShareTunnel::Starting;
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // `tunnel::start` is blocking (it spawns cloudflared and
                    // waits up to a minute for the URL) and shells out to curl,
                    // so it must not run on the UI thread. It needs no Tokio
                    // reactor of its own: nothing in it builds a reqwest client.
                    let state_dir = tunnel::default_state_dir();
                    match tunnel::status(&state_dir) {
                        Ok(Some(state)) => Ok(state.url),
                        Ok(None) => tunnel::start(&tunnel::TunnelSpec::new(port))
                            .map(|state| state.url)
                            .map_err(|e| format!("{e:#}")),
                        Err(e) => Err(format!("{e:#}")),
                    }
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                let label = this_host_label();
                let Some(share) = this.instance_share.as_mut() else {
                    return;
                };
                match result {
                    Ok(url) => apply_tunnel(share, url, &label),
                    Err(e) => share.tunnel = ShareTunnel::Failed(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the sharing card. The code stays valid until it expires: closing
    /// the card is not a revoke, and pretending otherwise would be a lie the
    /// store cannot back.
    pub(crate) fn instance_share_close(&mut self, cx: &mut Context<Self>) {
        self.instance_share = None;
        cx.notify();
    }

    /// The sharing card: what to hand the other crab, and what it cannot do.
    pub(crate) fn instance_share_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(share) = self.instance_share.clone() else {
            return div().into_any_element();
        };
        let shown = palette::current();

        // The QR is drawn white-on-black whatever the theme is: a scanner needs
        // contrast, and a dark card with dark modules is a pretty square that
        // no camera reads. Fixed colours, not palette ones.
        let qr = if share.width > 0 {
            let mut rows: Vec<AnyElement> = Vec::with_capacity(share.width);
            for y in 0..share.width {
                let mut cells: Vec<AnyElement> = Vec::with_capacity(share.width);
                for x in 0..share.width {
                    let dark = share.qr.get(y * share.width + x).copied().unwrap_or(false);
                    cells.push(
                        div()
                            .w(px(QR_MODULE_PX))
                            .h(px(QR_MODULE_PX))
                            .bg(rgb(if dark { 0x000000 } else { 0xffffff }))
                            .into_any_element(),
                    );
                }
                rows.push(div().flex().flex_row().children(cells).into_any_element());
            }
            // The card is centred by a row, not by `self_center`: gpui's `Div`
            // has no self-alignment, and a full-width row with `justify_center`
            // is the same thing with one element less to get wrong.
            div()
                .flex()
                .flex_row()
                .justify_center()
                .w_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(0xffffff))
                        .children(rows),
                )
                .into_any_element()
        } else {
            div()
                .text_size(rems(0.75))
                .text_color(rgb(shown.warn))
                .child(SharedString::from(
                    "não consegui desenhar o QR: usa o link copiado abaixo",
                ))
                .into_any_element()
        };

        let copy_link = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            if let Some(share) = this.instance_share.as_ref() {
                cx.write_to_clipboard(ClipboardItem::new_string(share.link.clone()));
            }
        });
        let copy_code = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            if let Some(share) = this.instance_share.as_ref() {
                cx.write_to_clipboard(ClipboardItem::new_string(share.code.clone()));
            }
        });
        let fresh = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_share_begin(cx);
        });
        let open_tunnel = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_share_tunnel(cx);
        });

        let button = |id: &'static str, label: &'static str, accent: bool| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(if accent { shown.accent } else { shown.panel_hi }))
                .text_size(rems(0.75))
                .text_color(rgb(if accent { shown.bg } else { shown.text }))
                .child(SharedString::from(label))
        };

        let mut body: Vec<AnyElement> = Vec::new();
        body.push(qr);
        body.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from("endereço deste crab")),
                )
                .child(
                    div()
                        .text_size(rems(0.8125))
                        .text_color(rgb(shown.text))
                        .child(SharedString::from(share.url.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(rems(0.6875))
                                .text_color(rgb(shown.text_faint))
                                .child(SharedString::from("código")),
                        )
                        .child(
                            div()
                                .text_size(rems(1.1))
                                .text_color(rgb(shown.accent))
                                .child(SharedString::from(share.code.clone())),
                        ),
                )
                .into_any_element(),
        );
        if let Some(warning) = &share.warning {
            body.push(
                div()
                    .text_size(rems(0.75))
                    .text_color(rgb(shown.warn))
                    .child(SharedString::from(warning.clone()))
                    .into_any_element(),
            );
        }
        // The fix, not just the diagnosis. The warning names keys; this is the
        // block already assembled with this machine's own address, so the owner
        // copies one thing instead of editing TOML from a sentence.
        if let Some(snippet) = &share.snippet {
            let copy_snippet = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                if let Some(share) = this.instance_share.as_ref()
                    && let Some(snippet) = share.snippet.as_ref()
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(snippet.clone()));
                }
            });
            body.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(0.6875))
                                    .text_color(rgb(shown.text_faint))
                                    .child(SharedString::from(
                                        "cola isto em config.toml e reabre a janela",
                                    )),
                            )
                            .child(
                                button("instance-share-copy-config", "copiar", false)
                                    .on_click(copy_snippet),
                            ),
                    )
                    .child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(shown.panel_hi))
                            .text_size(rems(0.6875))
                            .text_color(rgb(shown.text_normal))
                            .child(SharedString::from(snippet.clone())),
                    )
                    .into_any_element(),
            );
        }
        // The public tunnel, and the only way to get one from this window. It is
        // its own click because it publishes a URL on the internet; the label
        // says what it does, and the failure keeps the bind address on screen
        // instead of leaving an empty card.
        body.push(match &share.tunnel {
            ShareTunnel::Idle => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(
                            "noutra rede (ou fora de casa) o endereço acima não chega lá",
                        )),
                )
                .child(
                    button("instance-share-tunnel", "abrir túnel público", false)
                        .on_click(open_tunnel),
                )
                .into_any_element(),
            ShareTunnel::Starting => div()
                .text_size(rems(0.75))
                .text_color(rgb(shown.text_normal))
                .child(SharedString::from(
                    "a abrir o túnel público… (o primeiro arranque descarrega o cloudflared)",
                ))
                .into_any_element(),
            ShareTunnel::Up(url) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(rems(0.75))
                        .text_color(rgb(shown.green))
                        .child(SharedString::from(
                            "túnel público ativo: este endereço funciona de qualquer lado",
                        )),
                )
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(url.clone())),
                )
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_hint))
                        .child(SharedString::from(
                            "fica de pé depois de fechares o cartão; desliga-o com `opencrabs connect --stop`",
                        )),
                )
                .into_any_element(),
            ShareTunnel::Failed(why) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(rems(0.75))
                        .text_color(rgb(shown.warn))
                        .child(SharedString::from(format!("túnel: {why}"))),
                )
                .child(
                    button("instance-share-tunnel-retry", "tentar outra vez", false)
                        .on_click(open_tunnel),
                )
                .into_any_element(),
        });

        body.push(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(button("instance-share-copy-link", "copiar link", true).on_click(copy_link))
                .child(
                    button("instance-share-copy-code", "copiar código", false).on_click(copy_code),
                )
                .child(button("instance-share-new", "novo código", false).on_click(fresh))
                .into_any_element(),
        );

        // The instructions are the point of the card. A code with no "what do I
        // do with this" is a number the owner has to reverse-engineer, and the
        // other crab's flow has three steps that are not guessable from here.
        body.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pt_2()
                .border_t_1()
                .border_color(rgb(shown.track))
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from("como ligar o outro crab")),
                )
                .children(share_steps().iter().enumerate().map(|(i, step)| {
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .text_size(rems(0.75))
                                .text_color(rgb(shown.accent))
                                .child(SharedString::from(format!("{}.", i + 1))),
                        )
                        .child(
                            div()
                                .text_size(rems(0.75))
                                .text_color(rgb(shown.text))
                                .child(SharedString::from(*step)),
                        )
                        .into_any_element()
                }))
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(rgb(shown.text_faint))
                        .child(SharedString::from(SHARE_FOOTNOTE)),
                )
                .into_any_element(),
        );

        let close = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.instance_share_close(cx);
        });
        modal_shell::render(
            modal_shell::ModalView {
                id: "instance-share-modal",
                title: "Acesso remoto a este crab".to_string(),
                subtitle: "o outro crab lê este link e fica emparelhado".to_string(),
                actions: None,
                header_close: true,
                body: div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_h_0()
                    // The card grew: QR, address, code, warning, the config
                    // block, buttons and three steps do not fit a short window,
                    // and a column that paints past the card's edge covers the
                    // footer and the close button. Capped and scrollable, it
                    // clips where it should and the whole card stays reachable.
                    .max_h(px(21.0 * self.rem))
                    // Scroll is state in GPUI: `overflow_y_scroll` lives on
                    // `StatefulInteractiveElement`, so the div needs an id
                    // first or the method does not resolve at all.
                    .id("instance-share-body")
                    .overflow_y_scroll()
                    .children(body)
                    .into_any_element(),
                footer: Some(
                    modal_shell::footer(format!(
                        "expira em {} min · esc fecha · o link copiado é a via segura",
                        share.ttl_secs / 60
                    ))
                    .into_any_element(),
                ),
                width: 26.0 * self.rem,
                max_height: 30.0 * self.rem,
            },
            close,
        )
        .into_any_element()
    }
}

/// One QR module's side in pixels. 4 px over a 33-module code is 132 px, which
/// fits a card and is still big enough for a phone camera to lock on.
const QR_MODULE_PX: f32 = 4.0;

/// The steps the card prints, in the order the other crab needs them.
///
/// Pure and fixed so a test can pin them: the card's value is the instruction,
/// and an instruction that drifts from the flow the other crab actually has is
/// worse than no instruction at all. Each one names the exact control the owner
/// must press on the far side.
pub(crate) fn share_steps() -> [&'static str; 3] {
    [
        "no outro crab: botão direito no C, no topo da barra, e escolhe «emparelhar um crab»",
        "cola o link copiado: preenche o endereço, o código e o nome de uma vez",
        "carrega em «emparelhar»: o crab novo aparece na barra e as sessões dele abrem",
    ]
}

/// What the card says about the two things it cannot do yet.
///
/// Both are true today and both surprise people: the QR needs a client that
/// reads codes (two desktops paste the link), and discovery on the local
/// network does not exist, so a crab never appears without a code. Saying it
/// here costs one line; leaving it out costs an hour of "porque não aparece?".
pub(crate) const SHARE_FOOTNOTE: &str = "entre dois PCs o caminho é o link colado; o QR serve um cliente que saiba ler códigos. descoberta automática na rede ainda não existe: sem código, um crab não aparece na barra.";

/// Where the address on the card comes from.
///
/// The bind is the honest answer for a machine on the same network, and the
/// wrong answer for one that is not: a loopback gateway is unreachable, and a
/// LAN address dies the moment the other crab is somewhere else. `opencrabs
/// connect` already solves that with a cloudflared quick tunnel, so the card
/// offers the same path instead of telling the owner to edit TOML and restart
/// the window (2026-11-21, Daniel: "a parte de qr code nao sei como vai
/// funcionar isso a nivel do user").
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ShareTunnel {
    /// No tunnel asked for: the card shows the bind address and its warning.
    Idle,
    /// cloudflared is being resolved, downloaded or started.
    Starting,
    /// A quick tunnel is up. This URL works from anywhere.
    Up(String),
    /// The tunnel could not be opened; the bind address is all that is left.
    Failed(String),
}

/// The sharing card's model: everything the paint reads, built once when the
/// code is minted. The code exists in plaintext only here and in the store's
/// hash, which is why the card is rebuilt rather than re-read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShareView {
    /// The address another crab should dial.
    pub(crate) url: String,
    /// The single-use code.
    pub(crate) code: String,
    /// `opencrabs://pair?…`, which is what the QR carries.
    pub(crate) link: String,
    /// Row-major modules, `width` per side.
    pub(crate) qr: Vec<bool>,
    pub(crate) width: usize,
    /// Why this address will not work from another machine, when it will not.
    pub(crate) warning: Option<String>,
    /// The `[a2a]` block to paste when the config is what is in the way.
    pub(crate) snippet: Option<String>,
    /// Public tunnel state: the card is usable over the internet only when this
    /// is `Up`, and it says so instead of letting the owner assume it.
    pub(crate) tunnel: ShareTunnel,
    pub(crate) ttl_secs: u64,
}

/// The address the card prints, with a running tunnel taking precedence.
///
/// Split from `share_target` so the precedence is testable without a config
/// file, a store or a window: a tunnel that is up is an address another machine
/// can actually dial, and the bind address it forwards is not. The warning and
/// the TOML block exist only for the case where nothing is published, so a live
/// tunnel clears both instead of leaving two stale paragraphs on the card.
pub(crate) fn share_address(
    a2a: &opencrabs::config::A2aConfig,
    tunnel: Option<&tunnel::TunnelState>,
) -> (String, Option<String>, Option<String>) {
    if let Some(state) = tunnel {
        return (state.url.clone(), None, None);
    }
    let (url, warning) = share_target(a2a);
    (url, warning, share_config_snippet(a2a))
}

/// The address to hand out, and the reason it may be useless.
///
/// `advertise_url` wins because it is the owner's explicit answer to "how do I
/// reach this box". Without it the bind decides: a loopback gateway cannot be
/// reached from anywhere else, and saying "here is a code" while the listener
/// is bound to 127.0.0.1 is how an hour disappears into a pairing that could
/// never have worked.
fn share_target(a2a: &opencrabs::config::A2aConfig) -> (String, Option<String>) {
    if !a2a.enabled {
        return (
            format!("http://127.0.0.1:{}", a2a.port),
            Some(
                "o gateway A2A está desligado nesta máquina: põe [a2a] enabled = true e volta a abrir a janela."
                    .to_string(),
            ),
        );
    }
    if let Some(url) = a2a
        .advertise_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        return (url.trim_end_matches('/').to_string(), None);
    }
    let loopback = matches!(a2a.bind.as_str(), "127.0.0.1" | "localhost" | "::1");
    if loopback {
        return (
            format!("http://127.0.0.1:{}", a2a.port),
            Some(format!(
                "esta máquina só aceita ligações locais, por isso outro crab não chega aqui. Para abrir à rede: [a2a] bind = \"0.0.0.0\" e advertise_url = \"http://{}:{}\", depois fecha e reabre a janela.",
                lan_ip().unwrap_or_else(|| "o-ip-desta-máquina".to_string()),
                a2a.port
            )),
        );
    }
    let host = lan_ip().unwrap_or_else(|| "0.0.0.0".to_string());
    let warning = if a2a
        .api_key
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        Some(
            "sem api_key, qualquer crab na tua rede pode chamar /a2a/v1. Põe uma em [a2a] api_key."
                .to_string(),
        )
    } else {
        None
    };
    (format!("http://{host}:{}", a2a.port), warning)
}

/// The `[a2a]` block this machine needs before another crab can reach it, as
/// text the owner can paste into `config.toml`.
///
/// A warning that names two keys still leaves the owner to assemble them in the
/// right section with the right quotes, and a typo there is a window that will
/// not start. The snippet is that assembly done here: what is missing is filled
/// in with this machine's real address, what is already right is left alone,
/// and a machine that needs nothing gets `None` so a working card never shows a
/// config block it does not need.
pub(crate) fn share_config_snippet(a2a: &opencrabs::config::A2aConfig) -> Option<String> {
    let host = lan_ip().unwrap_or_else(|| "o-ip-desta-maquina".to_string());
    let loopback = matches!(a2a.bind.as_str(), "127.0.0.1" | "localhost" | "::1");
    let no_key = a2a
        .api_key
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty();
    if a2a.enabled && !loopback && !no_key {
        return None;
    }
    let mut lines = vec!["[a2a]".to_string()];
    if !a2a.enabled {
        lines.push("enabled = true".to_string());
    }
    if loopback {
        lines.push("bind = \"0.0.0.0\"".to_string());
    }
    if a2a
        .advertise_url
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        lines.push(format!("advertise_url = \"http://{host}:{}\"", a2a.port));
    }
    if no_key {
        lines.push("api_key = \"<uma-chave-longa-que-so-tu-sabes>\"".to_string());
    }
    Some(lines.join("\n"))
}

/// This machine's address on the LAN.
///
/// A UDP socket that never sends a packet: `connect` only picks the route, and
/// the local address it reports is the interface the kernel would use. Reading
/// it this way needs no crate and no DNS.
fn lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// What this crab calls itself in a pairing link, so the other side's strip
/// shows a name rather than a URL.
fn this_host_label() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "este crab".to_string())
}

/// The link as QR modules, row-major, and the side length.
///
/// A link that does not fit the largest QR version comes back empty rather
/// than as a half-drawn square: the card then shows the warning and the copy
/// button, which still works.
fn qr_modules(link: &str) -> (Vec<bool>, usize) {
    match qrcode::QrCode::new(link.as_bytes()) {
        Ok(code) => {
            let width = code.width();
            let dark = code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect();
            (dark, width)
        }
        Err(_) => (Vec::new(), 0),
    }
}

/// The remote session being read, and the draft going to it.
///
/// No `Clone`, no `PartialEq`, no `Debug`. It holds the same `TranscriptState`
/// the local chat paints from, and that type is neither clonable nor
/// comparable: it carries the loaded rows and their drawn turns. Nothing in the
/// window ever copies or compares two transcripts, so a derive would only be a
/// promise the type cannot keep.
pub(crate) struct RemoteTranscript {
    pub(crate) session_id: String,
    /// The A2A context this session is bound to, as the host reports it. A
    /// send carries it so the message joins this conversation; without it the
    /// host mints a context per message, which is a new session per message
    /// (#3133). `None` until the transcript has been read, and for a session
    /// that has never been spoken to over A2A.
    pub(crate) context_id: Option<String>,
    pub(crate) title: SharedString,
    /// The remote rows, already shaped by the transcript's own rules: the same
    /// state type the local chat paints from, so the two cannot diverge.
    pub(crate) state: transcript::TranscriptState,
    /// Whether anything in `state` is drawable. A session whose rows are all
    /// scaffolding has turns and still nothing to read.
    pub(crate) has_rows: bool,
    /// Why the body is empty or what the last send did, in one line.
    pub(crate) notice: Option<SharedString>,
    pub(crate) sending: bool,
    /// The body's own scroll, tracked by the transcript painter it is handed to.
    pub(crate) scroll: ScrollHandle,
}

/// The pairing form's state, which is also "the form is open".
///
/// The three fields are real [`TextField`]s, not `String`s: a field that only
/// appends is a field you cannot fix a typo in, and a URL pasted from a
/// terminal arrives with a trailing newline and a code that has to replace
/// what is already there. Caret, selection, Home/End and the clipboard chords
/// all come from the same engine the composer uses, so the two cannot drift
/// (2026-11-20: the form was three `div`s printing a `String`, and Ctrl+V did
/// nothing at all).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GlyphForm {
    /// The crab whose badge is being marked.
    pub(crate) host_id: String,
    /// Its label, so the modal can say which crab without a second lookup.
    pub(crate) label: String,
    pub(crate) field: TextField,
}

impl GlyphForm {
    pub(crate) fn new(host_id: String, label: String, current: Option<String>) -> Self {
        let mut field = TextField::new();
        if let Some(mark) = current.as_deref().and_then(normalise_glyph) {
            field.insert_text(&mark);
        }
        Self {
            host_id,
            label,
            field,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PairingForm {
    pub(crate) url: TextField,
    pub(crate) code: TextField,
    pub(crate) label: TextField,
    /// A submit is in flight; the button says so and refuses a second one.
    pub(crate) busy: bool,
    /// Which field the keyboard is in: 0 url, 1 code, 2 label.
    pub(crate) field: usize,
}

impl PairingForm {
    pub(crate) fn busy(mut self) -> Self {
        self.busy = true;
        self
    }

    /// The three fields, in the order Tab walks them.
    pub(crate) fn fields(&self) -> [(&'static str, &TextField); 3] {
        [
            ("URL do crab", &self.url),
            ("código de emparelhamento", &self.code),
            ("nome (opcional)", &self.label),
        ]
    }

    /// The field the keyboard is in, for the editing chords.
    pub(crate) fn focused(&mut self) -> &mut TextField {
        match self.field {
            0 => &mut self.url,
            1 => &mut self.code,
            _ => &mut self.label,
        }
    }

    /// Fill the form from a pairing link, which is what the other crab prints
    /// and what the QR carries. Returns whether the text was one.
    ///
    /// The link is the whole point of the QR: reading four fields off a screen
    /// and typing them is where a pairing dies. One paste, both fields.
    pub(crate) fn absorb_link(&mut self, text: &str) -> bool {
        let Some((url, code, label)) = parse_pair_link(text) else {
            return false;
        };
        self.url.set_text(&url);
        self.code.set_text(&code);
        if let Some(label) = label {
            self.label.set_text(&label);
        }
        self.field = 2;
        true
    }
}

/// `opencrabs://pair?url=…&code=…&label=…`, as the sharing modal prints it.
///
/// The scheme is the contract between the two crabs, so the parser is here
/// rather than in a frontend: a link read by the window and a link read by the
/// terminal must mean the same thing.
pub(crate) fn parse_pair_link(text: &str) -> Option<(String, String, Option<String>)> {
    let text = text.trim();
    let rest = text.strip_prefix("opencrabs://pair?")?;
    let mut url = None;
    let mut code = None;
    let mut label = None;
    for pair in rest.split('&') {
        let (key, value) = pair.split_once('=')?;
        let value = percent_decode(value);
        match key {
            "url" => url = Some(value),
            "code" => code = Some(value),
            "label" => label = Some(value),
            _ => {}
        }
    }
    Some((url?, code?, label.filter(|l| !l.is_empty())))
}

/// Percent-decode a query value. The encoder on the printing side is
/// [`percent_encode`], and they are the only two halves of this format.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode everything outside the unreserved set. A URL carries `:`,
/// `/` and `.`, a code is base32, and a label is whatever the owner typed:
/// only the last one can surprise us, and this keeps it from breaking the
/// query.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Point the card at a public tunnel URL.
///
/// Everything downstream follows from the address: the link, the QR, and the
/// two blocks that only existed because the bind was unreachable. Clearing them
/// here (rather than in the caller) is what keeps "túnel activo" and "esta
/// máquina só aceita ligações locais" from being on screen at the same time.
pub(crate) fn apply_tunnel(share: &mut ShareView, url: String, label: &str) {
    share.tunnel = ShareTunnel::Up(url.clone());
    share.url = url;
    share.warning = None;
    share.snippet = None;
    share.link = pair_link(&share.url, &share.code, label);
    let (qr, width) = qr_modules(&share.link);
    share.qr = qr;
    share.width = width;
}

/// Build the `opencrabs://pair?…` link the card hands out and the QR carries.
///
/// One function so the link the card shows and the link the parser accepts
/// cannot drift: [`parse_pair_link`] is its inverse, and a test walks the round
/// trip.
pub(crate) fn pair_link(url: &str, code: &str, label: &str) -> String {
    format!(
        "opencrabs://pair?url={}&code={}&label={}",
        percent_encode(url),
        percent_encode(code),
        percent_encode(label),
    )
}

/// What the panel shows for the selected remote.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteView {
    pub(crate) host_id: String,
    pub(crate) url: String,
    pub(crate) state: RemoteState,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RemoteState {
    /// The list is in flight.
    Loading,
    /// The crab answered with these sessions.
    Ready(Vec<instance_client::RemoteSession>),
    /// It did not answer, or it answered with an error. The string is what the
    /// human reads, so it is the real error, not a code.
    Failed(String),
}

impl RemoteView {
    pub(crate) fn loading(host_id: String) -> Self {
        Self {
            host_id,
            url: String::new(),
            state: RemoteState::Loading,
        }
    }

    pub(crate) fn ready(
        host_id: String,
        url: String,
        sessions: Vec<instance_client::RemoteSession>,
    ) -> Self {
        Self {
            host_id,
            url,
            state: RemoteState::Ready(sessions),
        }
    }

    pub(crate) fn failed(host_id: String, url: String, error: String) -> Self {
        Self {
            host_id,
            url,
            state: RemoteState::Failed(error),
        }
    }
}
