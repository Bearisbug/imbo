//! In-memory sliding-window limits for unauthenticated and per-device traffic.
//! Per-conversation and per-account message limits are counted in the
//! database inside the send transaction (DESIGN §16).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Limiter {
    windows: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limiter {
    /// Record one hit for `key`. Returns `Err(seconds_to_wait)` when `max`
    /// hits already happened within `window`.
    pub fn hit(&self, key: &str, max: usize, window: Duration) -> Result<(), u64> {
        let now = Instant::now();
        let mut map = self.windows.lock().unwrap();
        if map.len() > 50_000 {
            map.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(3600))
            });
        }
        let q = map.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
            q.pop_front();
        }
        if q.len() >= max {
            let oldest = *q.front().unwrap();
            let wait = window.saturating_sub(now.duration_since(oldest));
            return Err(wait.as_secs().max(1));
        }
        q.push_back(now);
        Ok(())
    }
}
