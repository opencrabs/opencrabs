//! Wiring for the instance strip: where the credentials are read, where a pick
//! lands, and the health probe loop.
//!
//! Kept out of `dashboard_rail.rs` because it answers a different question. The
//! rail lists the sessions of *this* crab; the strip decides which crab that is.
//! A pick does not draw a second rail or a second panel: it swaps the window's
//! [`Backend`] and asks the rail to refetch, so every list, menu and screen
//! reads the picked crab through the same operations it reads its own with.
//!
//! Two rules here:
//!
//! - **The disk is read on the UI thread exactly once**, when the window opens.
//!   `state/remote/instances.json` is a small file, but a read on every frame is
//!   how a rail refresh turns into a stall.
//! - **The network is never touched on the UI thread.** The refetch after a
//!   pick, the handshake and the probes all run off it and only their result
//!   comes back. A remote that accepts the connection and then never answers
//!   must not be able to freeze a window.

use super::*;
use crate::backend_glue::Backend;
use crate::instance_strip::{StripBadge, StripFrame};
use crate::instances::{Health, Instances};
use opencrabs::backend::Op;
use opencrabs::backend::cap;
use opencrabs::backend::hello::Hello;
use opencrabs::remote::instance_client::{self, Target};
use opencrabs::remote::instance_repoint;
use opencrabs::remote::instance_store::{InstanceFile, InstanceStore, KnownInstance, now};
use opencrabs::ui_core::palette::CrabPick;

/// How often the probe loop wakes. Five seconds is the same cadence the
/// channel chip already pays.
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a crab that stays down waits between two sweeps of its stored
/// routes. A sweep can cost a few timeouts (a LAN address from another
/// network), so it does not run on every five-second probe.
const RECOVER_INTERVAL_SECS: u64 = 60;

/// The event stream held open on one remote conversation.
pub(crate) struct RemoteWatch {
    /// The crab it reads, by identity: a forget and a fresh pair is another
    /// crab even under the same name.
    crab: usize,
    session: Uuid,
    /// `None` when the reader could not start; kept so the window does not
    /// retry on every frame.
    _watch: Option<crate::backend_glue::Watch>,
}

impl Dashboard {
    /// Read `state/remote/instances.json` into the window's model.
    ///
    /// A missing file is an empty strip, not an error: a fresh install has no
    /// remotes and must not open with a warning. A corrupt file is an error,
    /// and it is said out loud, because silently showing an empty strip when
    /// the owner paired three crabs is how a bug hides for a week.
    pub(crate) fn load_instances(&mut self, cx: &mut Context<Self>) {
        let store = InstanceStore::new(InstanceStore::default_path());
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

    /// A badge was clicked: the window now talks to that crab.
    ///
    /// Clicking the badge you are already on goes back to this window's own
    /// crab: the strip is a picker, and a picker whose only exit is the same
    /// picker is a trap.
    pub(crate) fn instance_picked(&mut self, host_id: &str, cx: &mut Context<Self>) {
        if self.instances.selected_host_id() == Some(host_id) {
            self.instance_clear(cx);
            return;
        }
        let Some(entry) = self.instances.get(host_id) else {
            return;
        };
        let backend = Backend::remote(&entry.known, &self.home_backend);
        if !self.instances.select(host_id) {
            return;
        }
        self.instance_switch(backend, cx);
    }

    /// Ctrl+Alt+Up/Down: open the crab `delta` places from the one on screen,
    /// wrapping. The walk runs on a copy so the pick goes through
    /// [`Self::instance_picked`] like a click, swap and all; landing on the
    /// crab already open (a column of one) leaves it open rather than taking
    /// the click's "back home" meaning.
    pub(crate) fn instance_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let mut walk = self.instances.clone();
        let Some(host_id) = walk.select_step(delta).map(str::to_string) else {
            return;
        };
        if self.instances.selected_host_id() == Some(host_id.as_str()) {
            return;
        }
        self.instance_picked(&host_id, cx);
    }

    /// Back to the local crab: the workspace mark at the top of the strip, or
    /// a second click on the open badge.
    pub(crate) fn instance_clear(&mut self, cx: &mut Context<Self>) {
        if self.instances.selected_host_id().is_none() {
            return;
        }
        self.instances.clear_selection();
        let home = self.home_backend.clone();
        self.instance_switch(home, cx);
    }

    /// Keep one event stream open on the remote conversation on screen, and
    /// none otherwise.
    ///
    /// The stream is how a remote turn's progress (the streamed answer, the
    /// reasoning, the tool calls) reaches the card, and its first read is what
    /// fills the facts the footer names the model and the directory from.
    /// Cheap per frame: one comparison unless the crab or the session moved.
    pub(crate) fn sync_remote_watch(&mut self) {
        let want = self.backend.remote_key().zip(self.chat_session);
        let have = self.remote_watch.as_ref().map(|w| (w.crab, w.session));
        if want == have {
            return;
        }
        // The old stream closes before the next one opens.
        self.remote_watch = None;
        let Some((crab, session)) = want else {
            return;
        };
        let watch = match self.backend.watch(session, self.sink.clone()) {
            Ok(watch) => watch,
            Err(e) => {
                tracing::warn!("events of remote session {session} not opened: {e}");
                None
            }
        };
        self.remote_watch = Some(RemoteWatch {
            crab,
            session,
            _watch: watch,
        });
    }

    /// Hand the window a different crab and reload what it shows from it.
    ///
    /// The swap itself is one assignment on the UI thread; the reload is the
    /// rail's own refetch, which runs off it, and the handshake that tells the
    /// menus what this crab lets the window do. Until that handshake lands the
    /// window holds none, which greys nothing out: a refusal still arrives as
    /// a notice, the way it did before the handshake existed.
    ///
    /// What the window keeps per crab is read again from the new one: the
    /// usage view for the period on screen, the mission control snapshot, the
    /// topic state and the channel chip. None of them is left showing the
    /// crab it left.
    fn instance_switch(&mut self, backend: Backend, cx: &mut Context<Self>) {
        self.leave_conversation_for_swap(cx);
        self.backend = backend;
        self.hello = None;
        self.instance_error = None;
        self.instance_epoch = self.instance_epoch.wrapping_add(1);
        // An approval card asks for the crab the window just left.
        self.approvals.clear();
        // The topic grouping, the closed set and the icons are per crab too.
        // The rail's refetch reads the rows and the pins only, so without
        // this the new crab's topics stood flat under the old crab's map.
        self.sessions.parents.clear();
        self.sessions.topics.clear();
        self.sessions.closed.clear();
        self.sessions.topic_icons.clear();
        self.sessions.topic_icon_files.clear();
        self.refresh_icon_images();
        self.refresh_rail(cx);
        self.refresh_topic_state(cx);
        self.fetch_hello(cx);
        self.reload_usage(cx);
        self.mc = None;
        self.mc_err = None;
        self.mc_loading = false;
        if self.screen == Screen::MissionControl {
            self.fetch_mission_control(cx);
        }
        self.channel_chip = None;
        self.chip_channel_names.clear();
        self.refresh_chip_names(cx);
        cx.notify();
    }

    /// Close the conversation on screen before the window takes another crab.
    ///
    /// The transcript, the turn card and the draft all belong to the crab
    /// being left: keeping them up drew this machine's chat under a remote
    /// crab's badge, and a send from that composer named a session the new
    /// crab has never seen. A turn still running is parked under its session,
    /// the way a conversation switch parks it, and the draft is filed under
    /// it too, so going back to that crab and that row returns both.
    fn leave_conversation_for_swap(&mut self, cx: &mut Context<Self>) {
        self.edit_row = None;
        let Some(from) = self.chat_state.session() else {
            return;
        };
        if self.turn.busy || !self.turn.live.is_empty() {
            self.stashed.insert(from, std::mem::take(&mut self.turn));
        } else {
            self.turn = Default::default();
        }
        self.metrics.pending.remove(&from);
        let draft = self.composer.editor.update(cx, |ed, cx| ed.take_draft(cx));
        self.composer.drafts.store(from, draft);
        self.composer
            .editor
            .update(cx, |ed, cx| ed.begin_conversation(cx));
        self.set_chat_session(None);
        self.sessions.selected = None;
        self.sync_composer_session(cx);
        self.chat_state.transcript.error = None;
        self.chat_state.transcript.turns = Vec::new();
        self.chat_state.transcript.rows = Vec::new();
        self.chat_state.transcript.hidden = 0;
        self.msg_menu = None;
        self.msg_menu_row = None;
        self.msg_menu_leaf = None;
        self.msg_select.cancel();
        self.forward_pick = None;
        self.forward_leaf = None;
        self.forward_query.clear();
        self.forward_cursor = None;
        logging::info(&format!("instances: left session {from} on a crab swap"));
    }

    /// Ask the crab the window holds what it speaks, and keep the answer.
    ///
    /// A reply for a crab the window has since left is dropped: the epoch
    /// moves on every swap, so a slow handshake cannot grey out the menus of
    /// the crab that replaced it.
    pub(crate) fn fetch_hello(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let epoch = self.instance_epoch;
        cx.spawn(async move |this, cx| {
            let reply = backend.call(Hello {}).await;
            this.update(cx, |this, cx| {
                if this.instance_epoch != epoch {
                    return;
                }
                match reply {
                    Ok(hello) => this.hello = Some(hello),
                    Err(e) => {
                        let notice = cap::notice(<Hello as Op>::NAME, &e);
                        logging::info(&format!("instances: handshake: {notice}"));
                        this.instance_error = Some(SharedString::from(notice));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The crabs as the rail's list reads them (#3178), or `None` with
    /// `[gui] instance_rail` off: then the rail mounts nothing new, not even
    /// the arrow beside its logo.
    pub(crate) fn crab_list_frame(&self) -> Option<StripFrame> {
        if !self.instance_rail {
            return None;
        }
        Some(StripFrame::from_model(
            &self.instances,
            now(),
            self.instance_error.clone(),
        ))
    }

    /// The rail's logo was clicked: fold or unfold the list of crabs.
    pub(crate) fn toggle_crab_list(&mut self, cx: &mut Context<Self>) {
        if !self.instance_rail {
            return;
        }
        self.crabs_open = !self.crabs_open;
        cx.notify();
    }

    /// A crab's row in the list was right-clicked: its own card (open,
    /// forget, a new address).
    pub(crate) fn instance_manage(&mut self, badge: &StripBadge, cx: &mut Context<Self>) {
        let badge = badge.clone();
        self.instance_strip
            .update(cx, |strip, cx| strip.open_manage(&badge, cx));
    }

    /// A Seeker row for the crabs (`instance_strip::seeker_items`): the same
    /// handler the rail's list walks for that job. Nothing happens with
    /// `[gui] instance_rail` off, the same gate the rows are listed behind.
    ///
    /// A switch to the crab already open leaves it open: a click on the open
    /// row means "back home", but a row that names the crab must not take
    /// the reader somewhere else.
    pub(crate) fn run_crab_pick(&mut self, pick: CrabPick, cx: &mut Context<Self>) {
        if !self.instance_rail {
            return;
        }
        match pick {
            CrabPick::Switch(None) => self.instance_clear(cx),
            CrabPick::Switch(Some(host_id)) => {
                if self.instances.selected_host_id() != Some(host_id.as_str()) {
                    self.instance_picked(&host_id, cx);
                }
            }
            CrabPick::Pair => self.instance_pair(cx),
            CrabPick::Forget(host_id) | CrabPick::Repoint(host_id) => {
                let badge = self.crab_list_frame().and_then(|frame| {
                    frame
                        .badges
                        .into_iter()
                        .find(|badge| badge.host_id == host_id)
                });
                if let Some(badge) = badge {
                    self.instance_manage(&badge, cx);
                }
            }
        }
    }

    /// The list's "emparelhar um crab" row: the pairing card.
    pub(crate) fn instance_pair(&mut self, cx: &mut Context<Self>) {
        self.instance_strip
            .update(cx, |strip, cx| strip.open_pair(cx));
    }

    /// The strip entity, mounted for its cards only: it has no box of its
    /// own and paints the card that is up over the window. The render mounts
    /// it only behind `[gui] instance_rail`, the only way a card can be
    /// opened.
    pub(crate) fn instance_strip_element(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let frame = StripFrame::from_model(&self.instances, now(), self.instance_error.clone());
        let view = self.instance_strip.clone();
        view.update(cx, |strip, cx| strip.set_frame(frame, cx));
        AnyView::from(view).into_any_element()
    }

    /// A pairing went through: the strip hands over the store as it now
    /// reads, already loaded off the UI thread, and the window opens the new
    /// crab.
    ///
    /// The crab the window held stays selected in the fresh model, so the
    /// pick below is a switch and not the second click that goes home. The
    /// one exception is a crab paired again while it is open: its old token
    /// is the one the window holds, so it is picked anew to take the new one.
    pub(crate) fn instance_paired(
        &mut self,
        file: &InstanceFile,
        host_id: &str,
        cx: &mut Context<Self>,
    ) {
        let held = self.instances.selected_host_id().map(str::to_string);
        self.instances = Instances::from_file(file);
        self.instance_error = None;
        if let Some(held) = held.as_deref()
            && held != host_id
        {
            self.instances.select(held);
        }
        self.instance_picked(host_id, cx);
        cx.notify();
    }

    /// A crab was forgotten: the strip hands over the store as it now reads.
    ///
    /// If the window was on that crab it goes home first, through the same
    /// swap a click on the mark makes, so nothing on screen keeps talking to
    /// a crab whose token was just thrown away.
    pub(crate) fn instance_forgotten(
        &mut self,
        file: &InstanceFile,
        host_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.instances.selected_host_id() == Some(host_id) {
            self.instance_clear(cx);
        }
        let held = self.instances.selected_host_id().map(str::to_string);
        self.instances = Instances::from_file(file);
        self.instance_error = None;
        if let Some(held) = held.as_deref() {
            self.instances.select(held);
        }
        cx.notify();
    }

    /// Fold a new value of `[gui] instance_rail` into the window.
    ///
    /// Switching it off while a remote crab is held goes home first, through
    /// the same swap a click on the mark makes: a hidden strip must not leave
    /// the window talking to a crab the reader can no longer see or leave.
    /// An open `+` card is closed too, so no invisible card keeps the keys.
    ///
    /// Switching it on reads the store, which the window skipped at open with
    /// the rail off (#3178), and starts the probe loop the first time: a user
    /// who never turns the rail on never pays for either.
    pub(crate) fn set_instance_rail(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.instance_rail == on {
            return;
        }
        if !on {
            self.instance_clear(cx);
            self.instance_strip
                .update(cx, |strip, cx| strip.close_card(cx));
            self.crabs_open = false;
        }
        self.instance_rail = on;
        if on {
            self.load_instances(cx);
            if !self.instance_probing {
                self.instance_probing = true;
                Self::spawn_instance_probe(cx);
            }
        }
        logging::info(&format!(
            "instances: instance rail switched {}",
            match on {
                true => "on",
                false => "off",
            }
        ));
        cx.notify();
    }

    /// Flip `[gui] instance_rail` from the settings modal: written to this
    /// machine's config, never through the held crab, because it is a window
    /// preference. A failed write leaves the window as it was and says so.
    pub(crate) fn toggle_instance_rail(&mut self, cx: &mut Context<Self>) -> Result<bool, String> {
        let next = !self.instance_rail;
        opencrabs::config::Config::write_key("gui", "instance_rail", &next.to_string())
            .map_err(|e| format!("could not save gui.instance_rail: {e}"))?;
        self.set_instance_rail(next, cx);
        Ok(next)
    }

    /// A paired crab was followed to a new address, by the probe loop or by
    /// the badge's card. The model takes the new record in place; if the
    /// window is on that crab, its backend is rebuilt at the new address and
    /// what it shows is read again, without leaving the conversation: it is
    /// the same crab, only the road to it changed.
    pub(crate) fn instance_repointed(&mut self, known: &KnownInstance, cx: &mut Context<Self>) {
        if !self.instances.repoint(known) {
            return;
        }
        logging::info(&format!(
            "instances: {} re-pointed to {}",
            known.host_id, known.url
        ));
        if self.instances.selected_host_id() == Some(known.host_id.as_str()) {
            self.backend = Backend::remote(known, &self.home_backend);
            self.instance_epoch = self.instance_epoch.wrapping_add(1);
            self.instance_error = None;
            self.refresh_rail(cx);
            self.fetch_hello(cx);
        }
        cx.notify();
    }

    /// Whether the strip's `+` card is up. While it is, it owns the keyboard.
    pub(crate) fn instance_card_open(&self, cx: &App) -> bool {
        self.instance_strip.read(cx).card_open()
    }

    /// Hand one key to the strip's card.
    pub(crate) fn instance_card_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        self.instance_strip
            .update(cx, |strip, cx| strip.card_key(ks, cx));
    }

    /// Start the probe loop. Called once from the window's constructor; the
    /// loop exits when the window dies, like every other loop there.
    pub(crate) fn spawn_instance_probe(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // When each crab's stored routes were last swept, so a crab that
            // stays down is swept once a minute and not on every probe.
            let mut swept: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            loop {
                cx.background_executor().timer(PROBE_INTERVAL).await;
                // Snapshot the targets on the UI thread: the model is not
                // shared state and the worker must not read it.
                // With the strip switched off no paired crab is probed: a
                // hidden feature must not keep talking to the network.
                let targets = match this.update(cx, |this, _cx| {
                    if !this.instance_rail {
                        return Vec::new();
                    }
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

                // `reqwest` needs a Tokio reactor and gpui's executor is not
                // one, so the probes run on the remote client's shared runtime.
                // `block_on` holds a background thread, never the UI thread.
                let results = cx
                    .background_executor()
                    .spawn(async move {
                        let probes = targets.into_iter().map(|(host_id, url, token)| async move {
                            let target = Target::new(&url, Some(&token));
                            let result = instance_client::probe(&target).await;
                            (host_id, result.ok())
                        });
                        instance_client::block_on(async move {
                            Ok(futures::future::join_all(probes).await)
                        })
                        .unwrap_or_default()
                    })
                    .await;

                let at = now();
                let followed = match this.update(cx, |this, cx| {
                    let mut came_up = Vec::new();
                    let mut down = Vec::new();
                    let mut changed = false;
                    for (host_id, latency) in results {
                        if latency.is_none() {
                            down.push(host_id.clone());
                        }
                        let before = this.instances.get(&host_id).map(|e| e.health);
                        this.instances.record_probe(&host_id, latency.ok_or(()), at);
                        let after = this.instances.get(&host_id).map(|e| e.health);
                        changed |= before != after;
                        // A crab that answered is a crab whose token still
                        // works, which is worth remembering across restarts.
                        if matches!(after, Some(Health::Up { .. })) && before != after {
                            came_up.push(host_id.clone());
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
                    (came_up, down)
                }) {
                    Ok(followed) => followed,
                    Err(_) => break,
                };
                let (came_up, mut down) = followed;
                down.retain(|host_id| {
                    let due = swept
                        .get(host_id)
                        .is_none_or(|last| at.saturating_sub(*last) >= RECOVER_INTERVAL_SECS);
                    if due {
                        swept.insert(host_id.clone(), at);
                    }
                    due
                });
                for host_id in &came_up {
                    swept.remove(host_id);
                }
                if came_up.is_empty() && down.is_empty() {
                    continue;
                }

                // A crab that came up announces its routes again, so the next
                // address change has fresh ones to follow; a crab that is down
                // is looked for on the routes it announced last. Both are the
                // store and the network, so both run off the UI thread.
                let moved = cx
                    .background_executor()
                    .spawn(async move { follow_addresses(&came_up, &down) })
                    .await;
                if moved.is_empty() {
                    continue;
                }
                if this
                    .update(cx, |this, cx| {
                        for known in &moved {
                            this.instance_repointed(known, cx);
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
}

/// Refresh the routes of the crabs that came up, and sweep the stored routes
/// of the ones that are down. Returns the crabs that were found at a new
/// address. Runs on a background thread, on the remote client's runtime.
fn follow_addresses(came_up: &[String], down: &[String]) -> Vec<KnownInstance> {
    let store = InstanceStore::new(InstanceStore::default_path());
    instance_client::block_on(async {
        for host_id in came_up {
            if let Err(e) = instance_repoint::refresh_routes(&store, host_id).await {
                logging::info(&format!(
                    "instances: routes of {host_id} not refreshed: {e:#}"
                ));
            }
        }
        let mut moved = Vec::new();
        for host_id in down {
            match instance_repoint::recover(&store, host_id).await {
                Ok(Some(known)) => moved.push(known),
                Ok(None) => {}
                Err(e) => logging::info(&format!("instances: could not look for {host_id}: {e:#}")),
            }
        }
        Ok(moved)
    })
    .unwrap_or_default()
}
