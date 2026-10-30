//! Pending select menus (#382) and modal forms (#383), keyed by interaction
//! id with a lazy TTL: a stale pick answers "expired" (#386) and is dropped
//! on the way out instead of being swept by a timer.
//!
//! The TTL alone does not bound these maps (#2193): it is consulted only when
//! someone asks for a specific id, and an id nobody ever clicks is never asked
//! for. Inserts therefore prune to [`PENDING_INTERACTION_CAP`], oldest first,
//! the same discipline `tool_group` uses for its groups.

use std::collections::HashMap;
use std::time::Instant;

use super::DiscordState;
use super::interactions::FormSpec;

/// Ceiling for unclicked menus and forms (#2193). One entry per unclicked menu
/// accumulated forever on a long-lived daemon: no cap, no sweep, and no metric
/// to notice it.
pub(crate) const PENDING_INTERACTION_CAP: usize = 64;

/// True once `created` is older than `ttl_hours`.
fn expired(created: &Instant, ttl_hours: f64) -> bool {
    created.elapsed().as_secs_f64() > ttl_hours * 3600.0
}

/// Drop the oldest entries until one more insert stays within the cap.
///
/// Runs on insert rather than from a sweep, so the bound holds by
/// construction: there is no timer that has to be started, and no path that
/// can register without pruning.
fn make_room<K: Clone + Eq + std::hash::Hash, V>(map: &mut HashMap<K, (Instant, V)>) {
    while map.len() >= PENDING_INTERACTION_CAP {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, (created, _))| *created)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
}

impl DiscordState {
    /// Register a pending select menu (#382). Prunes to
    /// [`PENDING_INTERACTION_CAP`] first, so an unclicked menu cannot pile up.
    pub(crate) async fn register_select(&self, id: String, options: Vec<String>) {
        let mut map = self.pending_selects.lock().await;
        make_room(&mut map);
        map.insert(id, (Instant::now(), options));
    }

    /// Take a pending select if it is still within `ttl_hours`; expired or
    /// unknown entries return None (and expired ones are dropped).
    pub(crate) async fn take_select(&self, id: &str, ttl_hours: f64) -> Option<Vec<String>> {
        let mut map = self.pending_selects.lock().await;
        let (created, _) = map.get(id)?;
        if expired(created, ttl_hours) {
            map.remove(id);
            return None;
        }
        map.remove(id).map(|(_, opts)| opts)
    }

    /// Register a pending modal form spec (#383). Same pruning as the menus.
    pub(crate) async fn register_form(&self, id: String, spec: FormSpec) {
        let mut map = self.pending_forms.lock().await;
        make_room(&mut map);
        map.insert(id, (Instant::now(), spec));
    }

    /// Fetch a pending form spec within TTL (kept until submitted so the
    /// button can be pressed once per open; submission consumes it).
    pub(crate) async fn get_form(&self, id: &str, ttl_hours: f64) -> Option<FormSpec> {
        let mut map = self.pending_forms.lock().await;
        let (created, _) = map.get(id)?;
        if expired(created, ttl_hours) {
            map.remove(id);
            return None;
        }
        map.get(id).map(|(_, spec)| spec.clone())
    }

    /// Consume a form spec on submission.
    pub(crate) async fn take_form(&self, id: &str) -> Option<FormSpec> {
        self.pending_forms.lock().await.remove(id).map(|(_, s)| s)
    }
}
