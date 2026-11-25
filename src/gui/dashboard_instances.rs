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
use crate::instance_strip::{self, StripFrame};
use crate::instances::{Health, Instances};
use opencrabs::backend::Op;
use opencrabs::backend::cap;
use opencrabs::backend::hello::Hello;
use opencrabs::remote::instance_client::{self, Target};
use opencrabs::remote::instance_store::{InstanceStore, now};

/// How often the probe loop wakes. Five seconds is the same cadence the
/// channel chip already pays.
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

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

    /// Hand the window a different crab and reload what it shows from it.
    ///
    /// The swap itself is one assignment on the UI thread; the reload is the
    /// rail's own refetch, which runs off it, and the handshake that tells the
    /// menus what this crab lets the window do. Until that handshake lands the
    /// window holds none, which greys nothing out: a refusal still arrives as
    /// a notice, the way it did before the handshake existed.
    fn instance_switch(&mut self, backend: Backend, cx: &mut Context<Self>) {
        self.backend = backend;
        self.hello = None;
        self.instance_error = None;
        self.instance_epoch = self.instance_epoch.wrapping_add(1);
        self.refresh_rail(cx);
        self.fetch_hello(cx);
        cx.notify();
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

    /// The strip as a cached view, handed one frame of plain data. `None`
    /// while the window knows no other crab and has nothing to warn about:
    /// a column holding only this window's own mark would be noise.
    pub(crate) fn instance_strip_element(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.instances.entries().is_empty() && self.instance_error.is_none() {
            return None;
        }
        let frame = StripFrame::from_model(&self.instances, now(), self.instance_error.clone());
        let view = self.instance_strip.clone();
        view.update(cx, |strip, cx| strip.set_frame(frame, cx));
        Some(
            AnyView::from(view)
                .cached(instance_strip::strip_root_style())
                .into_any_element(),
        )
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
                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        for (host_id, latency) in results {
                            let before = this.instances.get(&host_id).map(|e| e.health);
                            this.instances.record_probe(&host_id, latency.ok_or(()), at);
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
}
