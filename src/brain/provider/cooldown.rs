//! Per-provider cooldown registry.
//!
//! A 429 that declares a wait parks the provider until that moment; the chain
//! routes around it and the provider re-enters on its own.
//!
//! The registry is an INSTANCE owned by the chain that uses it, never a
//! process-wide static. Two chains (a test's, a reload's) must not share a
//! window, and a global would let parallel tests race each other's parks.
//!
//! The window is the number the provider declared for itself, uncapped: a
//! provider that says "retry after 157000 s" is out for that long, and no
//! config key or ceiling shortens it. A later 429 can only extend it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// provider name -> the instant it may be tried again.
#[derive(Default)]
pub struct CooldownRegistry {
    until: Mutex<HashMap<String, Instant>>,
}

impl CooldownRegistry {
    /// Park `provider` until `until`. A later, longer 429 extends the window;
    /// an earlier one cannot shorten it.
    pub fn park(&self, provider: &str, until: Instant) {
        let mut m = self.until.lock().unwrap();
        let slot = m.entry(provider.to_string()).or_insert(until);
        if until > *slot {
            *slot = until;
        }
    }

    /// Remaining window, or `None` when the provider is not cooling. An
    /// expired entry is evicted on read, which is the whole recovery
    /// mechanism: nothing has to clear it for the provider to come back.
    pub fn remaining(&self, provider: &str) -> Option<Duration> {
        let now = Instant::now();
        let mut m = self.until.lock().unwrap();
        match m.get(provider).copied() {
            Some(until) if until > now => Some(until - now),
            Some(_) => {
                m.remove(provider);
                None
            }
            None => None,
        }
    }

    /// True while `provider` is parked.
    pub fn is_cooling(&self, provider: &str) -> bool {
        self.remaining(provider).is_some()
    }
}
