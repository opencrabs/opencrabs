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
use crate::instances::{Health, Instances};
use crate::remote_panel::{self, RemotePanelFrame, RemoteRow};
use opencrabs::remote::instance_client::{self, Target};
use opencrabs::remote::instance_store::{InstanceStore, KnownInstance, now};

/// How often the probe loop wakes. Five seconds is the same cadence the
/// channel chip already pays, and it is the only clock the strip has: the
/// paint never asks for a probe, it only draws what the last one recorded.
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

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
            title: SharedString::from(title),
            rows: Vec::new(),
            notice: None,
            sending: false,
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            let fetch_url = url.clone();
            let fetch_id = session_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let target = Target::new(&fetch_url, Some(&token));
                    instance_client::transcript(&target, &fetch_id, 40).await
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
                    Ok((_, messages)) => {
                        if let Some(transcript) = this.remote_transcript.as_mut() {
                            transcript.rows =
                                messages.iter().map(RemoteRow::from_message).collect();
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
        let Some(form) = self.instance_pairing.as_mut() else {
            return false;
        };
        if form.busy {
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
                let back = ks.modifiers.shift;
                form.field = if back {
                    (form.field + 2) % 3
                } else {
                    (form.field + 1) % 3
                };
                cx.notify();
            }
            "backspace" => {
                form.pop();
                cx.notify();
            }
            _ => {
                if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt {
                    return true;
                }
                let typed = if ks.key == "space" {
                    Some(' ')
                } else {
                    ks.key_char.as_ref().and_then(|s| s.chars().next())
                };
                if let Some(ch) = typed {
                    form.push(ch);
                    cx.notify();
                }
            }
        }
        true
    }

    /// The pairing modal: the URL the other crab announces, the code it printed
    /// and a name for the strip. Nothing touches the network until Enter.
    pub(crate) fn instance_pair_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.instance_pairing.clone() else {
            return div().into_any_element();
        };
        let shown = palette::current();
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, (label, value)) in form.fields().iter().enumerate() {
            let here = form.field == index;
            body.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
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
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(shown.panel))
                            .border_1()
                            .border_color(rgb(if here { shown.accent } else { shown.track }))
                            .text_size(rems(0.8125))
                            .text_color(rgb(if value.is_empty() {
                                shown.text_faint
                            } else {
                                shown.text
                            }))
                            .child(SharedString::from(if value.is_empty() {
                                if here {
                                    "a escrever…".to_string()
                                } else {
                                    String::new()
                                }
                            } else {
                                value.to_string()
                            })),
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
                width: 26.0 * f32::from(self.rem),
                max_height: 26.0 * f32::from(self.rem),
            },
            dismiss,
        )
        .into_any_element()
    }

    /// The panel column: the banner, the transcript and the remote composer.
    pub(crate) fn remote_panel_element(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            rows: transcript.map(|t| t.rows.clone()).unwrap_or_default(),
            notice,
            sending: transcript.is_some_and(|t| t.sending),
        };
        let send = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            this.send_remote(cx);
        });
        let remove = cx.listener(|this, _ev: &ClickEvent, _window, cx| {
            let Some(host_id) = this
                .instance_remote
                .as_ref()
                .map(|r| r.host_id.clone())
                .or_else(|| this.instances.selected_host_id().map(str::to_string))
            else {
                return;
            };
            this.instance_remove(&host_id, cx);
        });
        // `text()` borrows the field, and the frame outlives this method: the
        // copy is what makes the draft's bytes the frame's own.
        let draft = SharedString::from(self.remote_draft.text().to_string());
        remote_panel::view(
            &frame,
            &draft,
            move |ev, window, app| send(ev, window, app),
            move |ev, window, app| remove(ev, window, app),
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
        if let Some(transcript) = self.remote_transcript.as_mut() {
            transcript.sending = true;
        }
        self.remote_draft = text_field::TextField::new();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let send_url = url.clone();
            let body = text.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let target = Target::new(&send_url, Some(&token));
                    instance_client::send(&target, &body).await
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                if let Some(transcript) = this.remote_transcript.as_mut() {
                    transcript.sending = false;
                    transcript.notice = Some(match &result {
                        Ok(task_id) => SharedString::from(format!("tarefa criada: {task_id}")),
                        Err(e) => SharedString::from(format!("não enviou: {e:#}")),
                    });
                }
                if result.is_err() {
                    // Give the words back: losing a typed message to a network
                    // error is the one thing a composer must never do.
                    this.remote_draft = text_field::TextField::from_text(&text);
                }
                cx.notify();
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

                let results = cx
                    .background_executor()
                    .spawn(async move {
                        let probes = targets.into_iter().map(|(host_id, url, token)| async move {
                            let target = Target::new(&url, Some(&token));
                            let result = instance_client::probe(&target).await;
                            (host_id, result)
                        });
                        futures::future::join_all(probes).await
                    })
                    .await;

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
        let url = entry.url().to_string();
        let token = entry.known.token.clone();
        self.instance_remote = Some(RemoteView::loading(host_id.clone()));

        cx.spawn(async move |this, cx| {
            let target_url = url.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let target = Target::new(&target_url, Some(&token));
                    instance_client::list_sessions(&target, 50).await
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
        let url = form.url.trim().to_string();
        let code = form.code.trim().to_string();
        let label = form.label.trim().to_string();
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
                            url,
                            code: String::new(),
                            label,
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
}

/// The remote session being read, and the draft going to it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteTranscript {
    pub(crate) session_id: String,
    pub(crate) title: SharedString,
    pub(crate) rows: Vec<RemoteRow>,
    /// Why the body is empty or what the last send did, in one line.
    pub(crate) notice: Option<SharedString>,
    pub(crate) sending: bool,
}

/// The pairing form's state, which is also "the form is open".
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PairingForm {
    pub(crate) url: String,
    pub(crate) code: String,
    pub(crate) label: String,
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
    pub(crate) fn fields(&self) -> [(&'static str, &str); 3] {
        [
            ("URL do crab", self.url.as_str()),
            ("código de emparelhamento", self.code.as_str()),
            ("nome (opcional)", self.label.as_str()),
        ]
    }

    /// Where a typed character goes. One writer, so the field the keyboard is
    /// in and the field the paint highlights cannot disagree.
    pub(crate) fn push(&mut self, ch: char) {
        match self.field {
            0 => self.url.push(ch),
            1 => self.code.push(ch),
            _ => self.label.push(ch),
        }
    }

    /// Backspace in the focused field.
    pub(crate) fn pop(&mut self) {
        match self.field {
            0 => {
                self.url.pop();
            }
            1 => {
                self.code.pop();
            }
            _ => {
                self.label.pop();
            }
        }
    }
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
