//! A tiny time-limited cache for slow system reads that the panel snapshot repeats on every
//! click (battery health, SSD health, display modes, the startup task).

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Ttl<T: Clone> {
    slot: Mutex<Option<(Instant, T)>>,
    ttl: Duration,
}

impl<T: Clone> Ttl<T> {
    pub const fn new(ttl: Duration) -> Self {
        Self { slot: Mutex::new(None), ttl }
    }

    /// The cached value while it is younger than the TTL, else a fresh `read()`.
    pub fn get(&self, read: impl FnOnce() -> T) -> T {
        if let Ok(slot) = self.slot.lock()
            && let Some((at, value)) = slot.as_ref()
            && at.elapsed() < self.ttl
        {
            return value.clone();
        }
        let value = read();
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some((Instant::now(), value.clone()));
        }
        value
    }

    /// Forget the value (after a change made by GrandHonor itself).
    pub fn clear(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = None;
        }
    }
}
