//! Serialises the process-global Telegram 429 cooldown across test modules.
//!
//! `rate_limit::GLOBAL_COOLDOWN` is a process-wide `RwLock<Option<Instant>>`.
//! The parallel test harness runs every module in one process, so a test that
//! arms a deadline (the plan-card propagation tests arm 7s-11s cooldowns
//! through the product failure path) can wipe or extend another test's armed
//! deadline between its `record_global_429` and its assertion. Observed as
//! `ack_gate_tracks_reset` failing `assert!(!reaction_ack_permitted())` in a
//! full-suite run while every test passed in isolation: the propagation tests
//! armed cooldowns in the same millisecond window. The governor
//! `registry_guard` covers peer registries only, so the cooldown needs its own
//! lock. Same disease as the theme generation counter (`theme_global_lock`),
//! same cure, with one difference: several of these tests await inside the
//! critical section (`wait_global_cooldown`, `acquire_global_permit`, mockito
//! servers), so this is a `tokio::sync::Mutex`, not a std one.
//!
//! Every test that reads or writes the global cooldown takes this guard after
//! `registry_guard`, in that order everywhere, so no lock-order inversion is
//! possible:
//!
//! ```ignore
//! let _guard = test_support::registry_guard().await;
//! let _cooldown = telegram_cooldown_lock::guard().await;
//! ```
//!
//! That is not a request, and it is no longer only a comment (#1854):
//! `crate::tests::telegram_cooldown_discipline_1854_test` reads the test
//! sources and fails the build when a test body reaches the cooldown or the
//! shared clock without holding this guard. If the rule changes, the needle
//! list in that scan changes in the same commit, or the discipline decays
//! back into prose.
//!
//! It also serializes swaps of the process-wide `Config` mirror, by history
//! rather than by name: every governor suite holds this guard around its
//! `Config::set_current` because a swap landing mid-body rewrites another
//! suite's bucket math (#2012). Any test that replaces `Config::current()`
//! takes it too, for the same reason: one mirror, one lock.

use tokio::sync::{Mutex, MutexGuard};

static COOLDOWN_LOCK: Mutex<()> = Mutex::const_new(());

/// Take exclusive access to the global 429 cooldown state for the test body.
pub async fn guard() -> MutexGuard<'static, ()> {
    COOLDOWN_LOCK.lock().await
}
